//! User-facing opt-in and native elevated helper setup.
use crate::ui;
use std::path::PathBuf;
use windows::{
    Win32::{
        Foundation::{CloseHandle, HWND},
        System::Threading::{
            GetCurrentProcessId, GetExitCodeProcess, OpenProcess, PROCESS_SYNCHRONIZE,
            WaitForSingleObject,
        },
        UI::{
            Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW},
            WindowsAndMessaging::*,
        },
    },
    core::PCWSTR,
};
pub fn enabled() -> bool {
    portputer_windows_input::setup::enabled()
}
pub fn protected() -> bool {
    std::env::current_exe()
        .ok()
        .is_some_and(|p| portputer_windows_input::win::protected_image(&p, "Portputer.exe"))
}
pub fn destination(on: bool) -> Option<PathBuf> {
    if on {
        Some(portputer_windows_input::win::install_dir().join("Portputer.exe"))
    } else {
        portputer_windows_input::setup::original_app()
    }
}

pub fn request(hwnd: HWND, on: bool) -> Result<bool, String> {
    if on && !enabled() {
        let warning = ui::wide(
            "Allow Portputer on Windows logon and other secure screens?\n\nThis installs a privileged helper so paired devices can enter credentials and interact with administrator prompts. Only allow devices you trust.",
        );
        let title = ui::wide("Portputer: Windows secure screens");
        if unsafe {
            MessageBoxW(
                Some(hwnd),
                PCWSTR(warning.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
            )
        } != IDYES
        {
            return Ok(false);
        }
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let helper = exe
        .parent()
        .ok_or("Missing application directory")?
        .join("portputer-input-service.exe");
    if !helper.is_file() {
        return Err("The lock-screen helper is missing. Reinstall Portputer to restore it.".into());
    }
    let sid = portputer_windows_input::win::process_sid(unsafe { GetCurrentProcessId() })
        .map_err(|e| e.to_string())?;
    let file = ui::wide(&helper.to_string_lossy());
    let verb = ui::wide("runas");
    let args = ui::wide(&format!(
        "{} {sid}",
        if on { "--enable" } else { "--disable" }
    ));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        hwnd,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(args.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut info).map_err(|e| e.to_string())?;
    }
    // UAC has completed; wait for native setup off the UI thread.
    let process = info.hProcess.0 as usize;
    std::thread::spawn(move || {
        let handle = windows::Win32::Foundation::HANDLE(process as *mut _);
        let mut code = 1;
        let result = unsafe {
            let done = WaitForSingleObject(handle, 30_000);
            let read = GetExitCodeProcess(handle, &mut code);
            let _ = CloseHandle(handle);
            done.0 == 0 && read.is_ok() && code == 0
        };
        ui::run_on_ui(move || crate::app::secure_input_finished(on, result));
    });
    Ok(true)
}
pub fn relaunch(on: bool) -> Result<(), String> {
    let Some(path) = destination(on) else {
        return Ok(());
    };
    if std::env::current_exe().ok().as_ref() == Some(&path) {
        return Ok(());
    }
    if !path.is_file() {
        return Err("The normal Portputer installation could not be found.".into());
    }
    std::process::Command::new(path)
        .args(["--wait-for-exit", &std::process::id().to_string()])
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}
pub fn wait_for_previous_process() {
    let args: Vec<_> = std::env::args().collect();
    if let Some(pid) = args
        .iter()
        .position(|x| x == "--wait-for-exit")
        .and_then(|i| args.get(i + 1))
        .and_then(|x| x.parse::<u32>().ok())
    {
        unsafe {
            if let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                WaitForSingleObject(process, 15_000);
                let _ = CloseHandle(process);
            }
        }
    }
}
