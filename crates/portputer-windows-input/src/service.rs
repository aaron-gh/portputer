use portputer_windows_input::{KeyInput, Reply, Request, take_frame, win::*};
use serde::Deserialize;
use std::{
    collections::HashMap,
    ffi::OsString,
    fs::File,
    io::{self, Write},
    os::windows::io::AsRawHandle,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
};
use windows_sys::Win32::{
    Foundation::*,
    Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW,
    Security::*,
    Storage::FileSystem::*,
    System::{Pipes::*, RemoteDesktop::*, StationsAndDesktops::*, Threading::*},
    UI::Input::KeyboardAndMouse::*,
};

const NAME: &str = "PortputerSecureInput";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    owner_sid: String,
    #[serde(default)]
    diagnostics: bool,
}

fn config() -> io::Result<Config> {
    let c: Config = serde_json::from_slice(&std::fs::read(install_dir().join("config.json"))?)?;
    if !c.owner_sid.starts_with("S-1-5-21-")
        || !c
            .owner_sid
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'S' || b == b'-')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid owner SID",
        ));
    }
    Ok(c)
}
fn require_system() -> io::Result<()> {
    if process_sid(unsafe { GetCurrentProcessId() })? != "S-1-5-18"
        || !protected_image(&std::env::current_exe()?, "portputer-input-service.exe")
    {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "requires SYSTEM and the protected installation",
        ))
    } else {
        Ok(())
    }
}

pub fn entry() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    match args.get(1).map(String::as_str){
        Some("--enable")=>{portputer_windows_input::setup::enable(args.get(2).ok_or("missing owner SID")?)?;},
        Some("--disable")=>{portputer_windows_input::setup::disable(args.get(2).ok_or("missing owner SID")?)?;},
        Some("--uninstall")=>{portputer_windows_input::setup::uninstall()?;},
        Some("--service")=>{require_system()?;service_dispatcher::start(NAME,ffi_service_main)?;},
        Some("--worker")=>{require_system()?;worker(args.get(2).ok_or("missing parent PID")?.parse()?)?;},
        Some("--probe")|Some("--probe-key")=>{
            let mut c=Connection::connect()?;
            let request=if args[1]=="--probe-key"{Request::Input{events:vec![KeyInput{vk:0,scan:0x2a,flags:8},KeyInput{vk:0,scan:0x2a,flags:10}]}}else{Request::Probe{}};
            let r=c.request(&request)?;println!("{}",serde_json::to_string(&r)?);
            if !r.ok{return Err("broker probe failed".into());}
        },
        _=>return Err("usage: portputer-input-service --service | --worker PARENT_PID | --probe | --probe-key".into())
    }
    Ok(())
}
define_windows_service!(ffi_service_main, service_main);
fn service_main(_: Vec<OsString>) {
    if let Err(e) = run_service() {
        eprintln!("input service failed: {e}");
    }
}

fn status(state: ServiceState, accepted: ServiceControlAccept) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: accepted,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::ZERO,
        process_id: None,
    }
}
fn run_service() -> Result<(), Box<dyn std::error::Error>> {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let handle = service_control_handler::register(NAME, move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            flag.store(true, Ordering::SeqCst);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })?;
    let config = config()?;
    enable_privilege("SeTcbPrivilege")?;
    enable_privilege("SeAssignPrimaryTokenPrivilege")?;
    enable_privilege("SeIncreaseQuotaPrivilege")?;
    handle.set_service_status(status(
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
    ))?;
    let mut children: HashMap<u32, HANDLE> = HashMap::new();
    while !stop.load(Ordering::SeqCst) {
        children.retain(|_, child| unsafe {
            if WaitForSingleObject(*child, 0) == WAIT_OBJECT_0 {
                CloseHandle(*child);
                false
            } else {
                true
            }
        });
        for session in owner_sessions(&config.owner_sid) {
            if let std::collections::hash_map::Entry::Vacant(entry) = children.entry(session) {
                if let Ok(child) = spawn_worker(session) {
                    entry.insert(child);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    handle.set_service_status(status(
        ServiceState::StopPending,
        ServiceControlAccept::empty(),
    ))?;
    // Workers hold a SYNCHRONIZE handle on this process and cleanly release
    // keys/close their pipes when it exits. No persistent orphan SYSTEM worker.
    for child in children.into_values() {
        unsafe {
            CloseHandle(child);
        }
    }
    handle.set_service_status(status(ServiceState::Stopped, ServiceControlAccept::empty()))?;
    Ok(())
}

fn enable_privilege(name: &str) -> io::Result<()> {
    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut id = std::mem::zeroed();
        let ok = LookupPrivilegeValueW(std::ptr::null(), wide(name).as_ptr(), &mut id);
        if ok == 0 {
            CloseHandle(token);
            return Err(io::Error::last_os_error());
        }
        let privileges = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: id,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        SetLastError(0);
        let ok = AdjustTokenPrivileges(
            token,
            0,
            &privileges,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        let error = GetLastError();
        CloseHandle(token);
        if ok == 0 || error != 0 {
            Err(io::Error::from_raw_os_error(error as i32))
        } else {
            Ok(())
        }
    }
}
fn owner_sessions(owner: &str) -> Vec<u32> {
    unsafe {
        let mut sessions = std::ptr::null_mut();
        let mut count = 0;
        let mut result = vec![];
        if WTSEnumerateSessionsW(std::ptr::null_mut(), 0, 1, &mut sessions, &mut count) == 0 {
            return result;
        }
        for s in std::slice::from_raw_parts(sessions, count as usize) {
            if s.SessionId == 0
                || !(s.State == WTSActive || s.State == WTSConnected || s.State == WTSDisconnected)
            {
                continue;
            }
            let mut token = std::ptr::null_mut();
            if WTSQueryUserToken(s.SessionId, &mut token) == 0 {
                continue;
            }
            // User tokens are used only to establish the owner, never passed to
            // the broker or retained. Read TOKEN_USER and compare the SID.
            let mut size = 0;
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut size);
            let mut data = vec![0u64; (size as usize).div_ceil(8)];
            if GetTokenInformation(token, TokenUser, data.as_mut_ptr().cast(), size, &mut size) != 0
            {
                let user = &*data.as_ptr().cast::<TOKEN_USER>();
                let mut text = std::ptr::null_mut();
                if windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW(
                    user.User.Sid,
                    &mut text,
                ) != 0
                {
                    let mut len = 0;
                    while *text.add(len) != 0 {
                        len += 1;
                    }
                    if String::from_utf16_lossy(std::slice::from_raw_parts(text, len)) == owner {
                        result.push(s.SessionId);
                    }
                    LocalFree(text.cast());
                }
            }
            CloseHandle(token);
        }
        WTSFreeMemory(sessions.cast());
        result
    }
}
fn spawn_worker(session: u32) -> io::Result<HANDLE> {
    unsafe {
        let mut token = std::ptr::null_mut();
        let mut primary = std::ptr::null_mut();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_SESSIONID,
            &mut token,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let duplicated = DuplicateTokenEx(
            token,
            TOKEN_ALL_ACCESS,
            std::ptr::null(),
            SecurityImpersonation,
            TokenPrimary,
            &mut primary,
        );
        CloseHandle(token);
        if duplicated == 0 {
            return Err(io::Error::last_os_error());
        }
        if SetTokenInformation(primary, TokenSessionId, (&session as *const u32).cast(), 4) == 0 {
            CloseHandle(primary);
            return Err(io::Error::last_os_error());
        }
        let exe = std::env::current_exe()?;
        let mut command = wide(&format!(
            "\"{}\" --worker {}",
            exe.display(),
            GetCurrentProcessId()
        ));
        let mut desktop = wide("winsta0\\default");
        let mut startup: STARTUPINFOW = std::mem::zeroed();
        startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        startup.lpDesktop = desktop.as_mut_ptr();
        let mut child: PROCESS_INFORMATION = std::mem::zeroed();
        let ok = CreateProcessAsUserW(
            primary,
            wide(&exe.to_string_lossy()).as_ptr(),
            command.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_NO_WINDOW,
            std::ptr::null(),
            std::ptr::null(),
            &startup,
            &mut child,
        );
        let error = io::Error::last_os_error();
        CloseHandle(primary);
        if ok == 0 {
            Err(error)
        } else {
            CloseHandle(child.hThread);
            Ok(child.hProcess)
        }
    }
}

struct Desktop {
    original: HDESK,
    current: HDESK,
    name: String,
}
impl Desktop {
    fn new() -> Self {
        Self {
            original: unsafe { GetThreadDesktop(GetCurrentThreadId()) },
            current: std::ptr::null_mut(),
            name: String::new(),
        }
    }
    fn update(&mut self) -> io::Result<bool> {
        unsafe {
            let desktop = OpenInputDesktop(
                0,
                0,
                DESKTOP_READOBJECTS
                    | DESKTOP_WRITEOBJECTS
                    | DESKTOP_SWITCHDESKTOP
                    | DESKTOP_JOURNALPLAYBACK,
            );
            if desktop.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mut name = [0u16; 256];
            let mut needed = 0;
            if GetUserObjectInformationW(
                desktop,
                UOI_NAME,
                name.as_mut_ptr().cast(),
                512,
                &mut needed,
            ) == 0
            {
                CloseDesktop(desktop);
                return Err(io::Error::last_os_error());
            }
            let name =
                String::from_utf16_lossy(&name[..name.iter().position(|c| *c == 0).unwrap_or(256)]);
            if self.name == name {
                CloseDesktop(desktop);
                return Ok(false);
            }
            if SetThreadDesktop(desktop) == 0 {
                CloseDesktop(desktop);
                return Err(io::Error::last_os_error());
            }
            if !self.current.is_null() {
                CloseDesktop(self.current);
            }
            self.current = desktop;
            self.name = name;
            Ok(true)
        }
    }
}
impl Drop for Desktop {
    fn drop(&mut self) {
        unsafe {
            SetThreadDesktop(self.original);
            if !self.current.is_null() {
                CloseDesktop(self.current);
            }
        }
    }
}
fn inject(events: &[KeyInput]) -> bool {
    let inputs: Vec<_> = events
        .iter()
        .map(|k| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: k.vk,
                    wScan: k.scan,
                    dwFlags: k.flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        })
        .collect();
    unsafe {
        SetLastError(0);
        let inserted = SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        );
        if inserted != inputs.len() as u32 {
            let error = GetLastError();
            let _=std::fs::write(install_dir().join("last-input-error.json"),
                serde_json::json!({"requested":inputs.len(),"inserted":inserted,"win32_error":error}).to_string());
        }
        inserted == inputs.len() as u32
    }
}
fn release(held: &mut HashMap<(u16, u16, u32), KeyInput>) {
    let events: Vec<_> = held.drain().map(|(_, k)| k.up()).collect();
    for chunk in events.chunks(64) {
        inject(chunk);
    }
}
fn authorize(pipe: &File, owner: &str) -> bool {
    let result = authorize_client(pipe, owner);
    if let Err(reason) = &result {
        // Fixed protected path, overwritten rather than growing indefinitely.
        // Diagnostic metadata only; never log requests, keys or typed text.
        let _ = std::fs::write(install_dir().join("last-authorization-error.txt"), reason);
    }
    result.is_ok()
}
fn authorize_client(pipe: &File, owner: &str) -> Result<(), String> {
    let mut pid = 0;
    let mut session = 0;
    if unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle(), &mut pid) } == 0 {
        return Err(format!("client PID lookup: {}", io::Error::last_os_error()));
    }
    if unsafe { GetNamedPipeClientSessionId(pipe.as_raw_handle(), &mut session) } == 0 {
        return Err(format!(
            "client session lookup: {}",
            io::Error::last_os_error()
        ));
    }
    if session_id().ok() != Some(session) {
        return Err("client session mismatch".into());
    }
    let sid = process_sid(pid).map_err(|e| format!("client SID lookup: {e}"))?;
    if sid != owner {
        return Err("client owner SID mismatch".into());
    }
    let image = process_image(pid).map_err(|e| format!("client image lookup: {e}"))?;
    if !protected_image(&image, "Portputer.exe")
        && !protected_image(&image, "portputer-input-service.exe")
    {
        return Err(format!(
            "client image outside protected installation: {}",
            image.display()
        ));
    }
    Ok(())
}
fn create_pipe(session: u32, owner: &str) -> io::Result<File> {
    unsafe {
        let acl = wide(&format!("O:SYG:SYD:P(A;;GA;;;SY)(A;;GRGW;;;{owner})"));
        let mut descriptor = std::ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            acl.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let pipe = CreateNamedPipeW(
            wide(&pipe_name(session)).as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            32768,
            32768,
            0,
            &sa,
        );
        let error = io::Error::last_os_error();
        LocalFree(descriptor);
        if pipe == INVALID_HANDLE_VALUE {
            Err(error)
        } else {
            Ok(file_from_handle(pipe))
        }
    }
}
fn worker(parent_pid: u32) -> io::Result<()> {
    if process_sid(parent_pid)? != "S-1-5-18"
        || !protected_image(&process_image(parent_pid)?, "portputer-input-service.exe")
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let parent = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, parent_pid) };
    if parent.is_null() {
        return Err(io::Error::last_os_error());
    }
    let config = config()?;
    let result = worker_loop(parent, &config.owner_sid, config.diagnostics);
    unsafe {
        CloseHandle(parent);
    }
    result
}
fn worker_loop(parent: HANDLE, owner: &str, diagnostics: bool) -> io::Result<()> {
    let session = session_id()?;
    let mut pipe = create_pipe(session, owner)?;
    let mut desktop = Desktop::new();
    let mut connected = false;
    let mut buffer = vec![];
    let mut held = HashMap::new();
    let mut last_input = Instant::now();
    while unsafe { WaitForSingleObject(parent, 0) } == WAIT_TIMEOUT {
        // Poll even while idle, so a lock transition cannot leave modifiers
        // logically held. Never transfer held keys from one desktop to another.
        if desktop.update().unwrap_or(false) {
            release(&mut held);
        }
        if last_input.elapsed() > Duration::from_secs(30) {
            release(&mut held);
        }
        if !connected {
            let ok = unsafe { ConnectNamedPipe(pipe.as_raw_handle(), std::ptr::null_mut()) };
            let error = unsafe { GetLastError() };
            if ok != 0 || error == ERROR_PIPE_CONNECTED {
                connected = authorize(&pipe, owner);
                if !connected {
                    unsafe {
                        DisconnectNamedPipe(pipe.as_raw_handle());
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        let mut bytes = [0u8; 2048];
        let mut disconnect = false;
        match read_available(&mut pipe, &mut bytes) {
            Ok(0) => {}
            Ok(n) => buffer.extend_from_slice(&bytes[..n]),
            Err(e) if no_data(&e) => {}
            Err(_) => disconnect = true,
        }
        loop {
            let frame = match take_frame(&mut buffer) {
                Ok(Some(f)) => f,
                Ok(None) => break,
                Err(_) => {
                    disconnect = true;
                    break;
                }
            };
            let request = match serde_json::from_slice::<Request>(&frame) {
                Ok(r) if r.valid() => r,
                _ => {
                    disconnect = true;
                    break;
                }
            };
            let ready = desktop.update().is_ok();
            let ok = match request {
                Request::Probe {} => ready,
                Request::Release {} => {
                    release(&mut held);
                    ready
                }
                Request::Input { events } => {
                    last_input = Instant::now();
                    let ok = ready && inject(&events);
                    if diagnostics {
                        let mut pid = 0;
                        unsafe {
                            GetNamedPipeClientProcessId(pipe.as_raw_handle(), &mut pid);
                        }
                        let _=std::fs::write(install_dir().join("last-input-status.json"),
                            serde_json::json!({"ok":ok,"desktop":desktop.name,"session":session,"event_count":events.len(),"client_pid":pid}).to_string());
                    }
                    // On partial delivery, retain all attempted downs and do
                    // not forget an up whose delivery is uncertain.
                    for k in &events {
                        if ok && k.flags & 2 != 0 {
                            held.remove(&k.identity());
                        } else if ready && k.flags & 2 == 0 {
                            held.insert(k.identity(), *k);
                        }
                    }
                    ok
                }
            };
            let reply = Reply {
                ok,
                desktop: desktop.name.clone(),
                session,
            };
            let mut response = serde_json::to_vec(&reply)?;
            response.push(b'\n');
            if pipe.write_all(&response).is_err() {
                disconnect = true;
                break;
            }
        }
        if disconnect {
            release(&mut held);
            unsafe {
                DisconnectNamedPipe(pipe.as_raw_handle());
            }
            connected = false;
            buffer.clear();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    release(&mut held);
    unsafe {
        DisconnectNamedPipe(pipe.as_raw_handle());
    }
    Ok(())
}
