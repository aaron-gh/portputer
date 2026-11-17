//! Native opt-in installation, invoked by the packaged helper after UAC.
use crate::win::{install_dir, wide};
use std::{
    ffi::OsString,
    path::PathBuf,
    time::{Duration, Instant},
};
use windows_service::{
    service::{
        ServiceAccess, ServiceErrorControl, ServiceInfo, ServiceStartType, ServiceState,
        ServiceType,
    },
    service_manager::{ServiceManager, ServiceManagerAccess},
};
use windows_sys::Win32::{Foundation::*, Security::Authorization::*, Security::*};
pub const NAME: &str = "PortputerSecureInput";
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const FILES: &[&str] = &[
    "Portputer.exe",
    "portputer-input-service.exe",
    "WinSparkle.dll",
    "nvdaControllerClient.dll",
];

fn manager() -> Result<ServiceManager> {
    Ok(ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )?)
}
fn info(start_type: ServiceStartType) -> ServiceInfo {
    ServiceInfo {
        name: NAME.into(),
        display_name: "Portputer lock and secure screen control".into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type,
        error_control: ServiceErrorControl::Normal,
        executable_path: install_dir().join("portputer-input-service.exe"),
        launch_arguments: vec!["--service".into()],
        dependencies: vec![],
        account_name: None,
        account_password: None,
    }
}
pub fn enabled() -> bool {
    ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .ok()
        .and_then(|m| {
            m.open_service(
                NAME,
                ServiceAccess::QUERY_STATUS | ServiceAccess::QUERY_CONFIG,
            )
            .ok()
        })
        .is_some_and(|s| {
            s.query_status()
                .is_ok_and(|x| x.current_state == ServiceState::Running)
                && s.query_config()
                    .is_ok_and(|x| x.start_type != ServiceStartType::Disabled)
        })
}
pub fn original_app() -> Option<PathBuf> {
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(install_dir().join("installation.json")).ok()?)
            .ok()?;
    Some(PathBuf::from(value.get("original_dir")?.as_str()?).join("Portputer.exe"))
}
fn protect(path: &std::path::Path) -> Result<()> {
    unsafe {
        let mut descriptor = std::ptr::null_mut();
        let sddl = wide("O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FRFX;;;BU)");
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut present = 0;
        let mut dacl = std::ptr::null_mut();
        let mut defaulted = 0;
        if GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted) == 0 {
            LocalFree(descriptor);
            return Err(std::io::Error::last_os_error().into());
        }
        let mut owner = std::ptr::null_mut();
        if GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted) == 0 {
            LocalFree(descriptor);
            return Err(std::io::Error::last_os_error().into());
        }
        let mut name = wide(&path.to_string_lossy());
        let error = SetNamedSecurityInfoW(
            name.as_mut_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION,
            owner,
            std::ptr::null_mut(),
            dacl,
            std::ptr::null_mut(),
        );
        LocalFree(descriptor);
        if error != 0 {
            return Err(std::io::Error::from_raw_os_error(error as i32).into());
        }
        Ok(())
    }
}
fn stop(service: &windows_service::service::Service) -> Result<()> {
    if service.query_status()?.current_state == ServiceState::Stopped {
        return Ok(());
    }
    service.stop()?;
    let until = Instant::now() + Duration::from_secs(15);
    while service.query_status()?.current_state != ServiceState::Stopped {
        if Instant::now() > until {
            return Err("Timed out stopping lock-screen support".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_millis(600));
    Ok(())
}
fn check_owner(owner: &str) -> Result<()> {
    if !owner.starts_with("S-1-5-21-")
        || !owner
            .bytes()
            .all(|c| c.is_ascii_digit() || c == b'S' || c == b'-')
    {
        return Err("Invalid Windows user SID".into());
    }
    if let Ok(bytes) = std::fs::read(install_dir().join("config.json")) {
        let config: serde_json::Value = serde_json::from_slice(&bytes)?;
        if config.get("owner_sid").and_then(|x| x.as_str()) != Some(owner) {
            return Err("Lock-screen support belongs to another Windows account".into());
        }
    }
    Ok(())
}
pub fn enable(owner: &str) -> Result<()> {
    check_owner(owner)?;
    let manager = manager()?;
    let source = std::env::current_exe()?
        .parent()
        .ok_or("Missing package directory")?
        .to_path_buf();
    let target = install_dir();
    let existing = manager.open_service(NAME, ServiceAccess::ALL_ACCESS).ok();
    if let Some(s) = &existing {
        stop(s)?;
    }
    std::fs::create_dir_all(&target)?;
    protect(&target)?;
    if source != target {
        for name in &["Portputer.exe", "portputer-input-service.exe"] {
            if !source.join(name).is_file() {
                return Err(format!("Missing packaged {name}").into());
            }
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis()
            .to_string();
        let backup = target.join("Backups").join(stamp);
        std::fs::create_dir_all(&backup)?;
        for name in FILES
            .iter()
            .copied()
            .chain(["config.json", "installation.json"])
        {
            if target.join(name).is_file() {
                std::fs::copy(target.join(name), backup.join(name))?;
            }
        }
        for name in FILES {
            if source.join(name).is_file() {
                std::fs::copy(source.join(name), target.join(name))?;
                protect(&target.join(name))?;
            }
        }
        std::fs::write(
            target.join("installation.json"),
            serde_json::to_vec(&serde_json::json!({"original_dir": source}))?,
        )?;
        protect(&target.join("installation.json"))?;
    }
    std::fs::write(
        target.join("config.json"),
        serde_json::to_vec(&serde_json::json!({"owner_sid": owner, "diagnostics": false}))?,
    )?;
    protect(&target.join("config.json"))?;
    let service = match existing {
        Some(s) => {
            s.change_config(&info(ServiceStartType::AutoStart))?;
            s
        }
        None => manager.create_service(
            &info(ServiceStartType::AutoStart),
            ServiceAccess::ALL_ACCESS,
        )?,
    };
    firewall(true)?;
    service.start(&[] as &[OsString])?;
    let until = Instant::now() + Duration::from_secs(10);
    while service.query_status()?.current_state != ServiceState::Running {
        if Instant::now() > until {
            return Err("Lock-screen support did not start".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}
pub fn disable(owner: &str) -> Result<()> {
    check_owner(owner)?;
    let manager = manager()?;
    let service = manager.open_service(NAME, ServiceAccess::ALL_ACCESS)?;
    stop(&service)?;
    service.change_config(&info(ServiceStartType::Disabled))?;
    firewall(false)?;
    Ok(())
}
pub fn uninstall() -> Result<()> {
    let manager = manager()?;
    if let Ok(service) = manager.open_service(NAME, ServiceAccess::ALL_ACCESS) {
        stop(&service)?;
        service.delete()?;
    }
    firewall(false)?;
    let source = std::env::current_exe()?;
    for name in FILES.iter().copied().chain([
        "config.json",
        "installation.json",
        "last-input-status.json",
        "last-input-error.json",
        "last-authorization-error.txt",
    ]) {
        let path = install_dir().join(name);
        if path != source && path.is_file() {
            std::fs::remove_file(path)?;
        }
    }
    // Retain native-installer backups. Never change pairing/user data.
    Ok(())
}

fn firewall(on: bool) -> Result<()> {
    use std::os::windows::process::CommandExt;
    let tool = PathBuf::from(std::env::var_os("WINDIR").ok_or("Missing Windows directory")?)
        .join("System32/netsh.exe");
    let program = format!("program={}", install_dir().join("Portputer.exe").display());
    let _ = std::process::Command::new(&tool)
        .args([
            "advfirewall",
            "firewall",
            "delete",
            "rule",
            "name=Portputer secure screen control",
            &program,
        ])
        .creation_flags(0x08000000)
        .output()?;
    if on {
        let result = std::process::Command::new(tool)
            .args([
                "advfirewall",
                "firewall",
                "add",
                "rule",
                "name=Portputer secure screen control",
                "dir=in",
                "action=allow",
                &program,
                "enable=yes",
                "profile=any",
                "protocol=any",
                "remoteip=LocalSubnet,100.64.0.0/10,fd7a:115c:a1e0::/48",
            ])
            .creation_flags(0x08000000)
            .output()?;
        if !result.status.success() {
            return Err("Windows could not add the scoped Portputer firewall rule".into());
        }
    }
    Ok(())
}
