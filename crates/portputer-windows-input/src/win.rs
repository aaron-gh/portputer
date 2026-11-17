//! Windows local transport and identity checks, shared by the app and broker.
use crate::{KeyInput, MAX_FRAME, Reply, Request, take_frame};
use std::{
    cell::RefCell,
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::windows::{
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle},
    },
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Security::Authorization::ConvertSidToStringSidW,
    Security::*,
    System::{Pipes::*, RemoteDesktop::ProcessIdToSessionId, Threading::*},
};

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
pub fn session_id() -> io::Result<u32> {
    let mut id = 0;
    if unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut id) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(id)
    }
}
pub fn pipe_name(session: u32) -> String {
    format!(r"\\.\pipe\PortputerSecureInput-v1-{session}")
}

pub fn process_sid(pid: u32) -> io::Result<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut token = std::ptr::null_mut();
        let opened = OpenProcessToken(process, TOKEN_QUERY, &mut token);
        CloseHandle(process);
        if opened == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut size = 0;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut size);
        let mut data = vec![0u64; (size as usize).div_ceil(8)];
        let ok = GetTokenInformation(token, TokenUser, data.as_mut_ptr().cast(), size, &mut size);
        CloseHandle(token);
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let user = &*data.as_ptr().cast::<TOKEN_USER>();
        let mut sid = std::ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut sid) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut len = 0;
        while *sid.add(len) != 0 {
            len += 1;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(sid, len));
        LocalFree(sid.cast());
        Ok(text)
    }
}
pub fn process_image(pid: u32) -> io::Result<std::path::PathBuf> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut path = vec![0u16; 32768];
        let mut len = path.len() as u32;
        let ok = QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut len);
        CloseHandle(process);
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(String::from_utf16_lossy(&path[..len as usize]).into())
        }
    }
}

pub fn nonblocking(file: &File) -> io::Result<()> {
    let mode = PIPE_READMODE_BYTE | PIPE_NOWAIT;
    if unsafe {
        SetNamedPipeHandleState(
            file.as_raw_handle(),
            &mode,
            std::ptr::null(),
            std::ptr::null(),
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
pub fn no_data(e: &io::Error) -> bool {
    e.raw_os_error() == Some(ERROR_NO_DATA as i32) || e.kind() == io::ErrorKind::WouldBlock
}

/// A nonblocking byte pipe may return a successful zero-byte read when idle.
/// Peek distinguishes idle from a disconnected peer, avoiding false EOFs.
pub fn read_available(file: &mut File, buffer: &mut [u8]) -> io::Result<usize> {
    let mut available = 0;
    if unsafe {
        PeekNamedPipe(
            file.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut available,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if available == 0 {
        return Ok(0);
    }
    let count = buffer.len().min(available as usize);
    file.read(&mut buffer[..count])
}

pub struct Connection {
    file: File,
    buffer: Vec<u8>,
}
impl Connection {
    pub fn connect() -> io::Result<Self> {
        let session = session_id()?;
        // Identification-only SQOS: a fake pipe server cannot impersonate the
        // caller. Verify explicit SYSTEM ownership and the same session
        // before sending ANY input (which may contain passwords).
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(0x00100000 | 0x00010000)
            .open(pipe_name(session))
            .map_err(|e| io::Error::new(e.kind(), format!("open broker pipe: {e}")))?;
        let mut pid = 0;
        let mut actual_session = 0;
        if unsafe { GetNamedPipeServerProcessId(file.as_raw_handle(), &mut pid) } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "cannot identify pipe server: {}",
                    io::Error::last_os_error()
                ),
            ));
        }
        if unsafe { GetNamedPipeServerSessionId(file.as_raw_handle(), &mut actual_session) } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("query broker pipe session: {}", io::Error::last_os_error()),
            ));
        }
        if actual_session != session {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("broker session mismatch: {actual_session}, expected {session}"),
            ));
        }
        if !system_owned_pipe(&file)
            .map_err(|e| io::Error::new(e.kind(), format!("verify broker pipe owner: {e}")))?
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "input broker pipe is not SYSTEM-owned",
            ));
        }
        // The explicitly SYSTEM-owned pipe is the server trust boundary. A
        // standard user cannot query arbitrary SYSTEM process image/token
        // handles, nor create this owner. SYSTEM already has full trust on the
        // machine. The worker itself requires the protected executable path.
        nonblocking(&file)
            .map_err(|e| io::Error::new(e.kind(), format!("set client pipe mode: {e}")))?;
        Ok(Self {
            file,
            buffer: Vec::new(),
        })
    }
    pub fn request(&mut self, request: &Request) -> io::Result<Reply> {
        if !request.valid() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid input"));
        }
        let mut data = serde_json::to_vec(request)?;
        data.push(b'\n');
        if data.len() > MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "input frame too large",
            ));
        }
        // Frames are <=16KB; timeout all partial writes and reads. Never replay
        // after a write succeeds: the peer may have injected before disconnect.
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut sent = 0;
        while sent < data.len() {
            match self.file.write(&data[sent..]) {
                Ok(0) => return Err(io::ErrorKind::BrokenPipe.into()),
                Ok(n) => sent += n,
                Err(e) if no_data(&e) => {}
                Err(e) => return Err(e),
            }
            if Instant::now() > deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            if sent < data.len() {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        loop {
            if let Some(frame) = take_frame(&mut self.buffer)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
            {
                return Ok(serde_json::from_slice(&frame)?);
            }
            let mut chunk = [0; 2048];
            match read_available(&mut self.file, &mut chunk) {
                Ok(0) => {}
                Ok(n) => self.buffer.extend_from_slice(&chunk[..n]),
                Err(e) if no_data(&e) => {}
                Err(e) => return Err(e),
            }
            if Instant::now() > deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn system_owned_pipe(file: &File) -> io::Result<bool> {
    // A normal user can query a pipe's owner without opening a SYSTEM process
    // token. The service explicitly sets O:SY; an unprivileged squatter cannot
    // create a SYSTEM-owned object. Keep the session and protected-image checks.
    unsafe {
        let mut owner = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        let error = windows_sys::Win32::Security::Authorization::GetSecurityInfo(
            file.as_raw_handle(),
            windows_sys::Win32::Security::Authorization::SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        );
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        let system = !owner.is_null() && IsWellKnownSid(owner, WinLocalSystemSid) != 0;
        LocalFree(descriptor);
        Ok(system)
    }
}

/// The installer creates this directory with only Administrators and SYSTEM
/// write access. A per-user install is deliberately not trusted for elevation.
pub fn install_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(
        std::env::var_os("ProgramFiles").unwrap_or_else(|| "C:\\Program Files".into()),
    )
    .join("Portputer Secure Input")
}
pub fn protected_image(path: &std::path::Path, name: &str) -> bool {
    path.to_string_lossy()
        .eq_ignore_ascii_case(&install_dir().join(name).to_string_lossy())
}

struct Bridge {
    connection: Option<Connection>,
    retry_at: Instant,
}
thread_local! { static BRIDGE: RefCell<Bridge> = RefCell::new(Bridge { connection:None,retry_at:Instant::now() }); }

/// None means no broker accepted a connection, so ordinary input may be used.
/// Some(false) includes ambiguous delivery: callers MUST NOT replay the batch.
pub fn try_send(events: &[KeyInput]) -> Option<bool> {
    BRIDGE.with(|cell| {
        let mut bridge = cell.borrow_mut();
        if bridge.connection.is_none() {
            if Instant::now() < bridge.retry_at {
                return None;
            }
            bridge.retry_at = Instant::now() + Duration::from_secs(1);
            bridge.connection = Connection::connect().ok();
        }
        let connection = bridge.connection.as_mut()?;
        match connection.request(&Request::Input {
            events: events.to_vec(),
        }) {
            Ok(reply) => Some(reply.ok),
            Err(_) => {
                bridge.connection = None;
                Some(false)
            }
        }
    })
}

/// Transfers ownership of a newly created Win32 file/pipe handle to `File`.
///
/// # Safety
/// `handle` must be a valid, uniquely owned file/pipe handle, not a borrowed
/// handle or INVALID_HANDLE_VALUE. The caller must not close it afterwards.
pub unsafe fn file_from_handle(handle: HANDLE) -> File {
    unsafe { File::from_raw_handle(handle) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
    };
    #[test]
    fn idle_pipe_is_not_eof_and_disconnect_is_detected() {
        let name = format!("{}-idle-test", pipe_name(session_id().unwrap()));
        let handle = unsafe {
            CreateNamedPipeW(
                wide(&name).as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                32768,
                32768,
                0,
                std::ptr::null(),
            )
        };
        assert_ne!(handle, INVALID_HANDLE_VALUE);
        let mut server = unsafe { file_from_handle(handle) };
        let mut client = OpenOptions::new()
            .read(true)
            .write(true)
            .open(name)
            .unwrap();
        unsafe {
            ConnectNamedPipe(server.as_raw_handle(), std::ptr::null_mut());
        }
        let mut buffer = [0u8; 32];
        assert_eq!(read_available(&mut server, &mut buffer).unwrap(), 0);
        client.write_all(b"frame\n").unwrap();
        assert_eq!(read_available(&mut server, &mut buffer).unwrap(), 6);
        assert_eq!(&buffer[..6], b"frame\n");
        drop(client);
        assert!(read_available(&mut server, &mut buffer).is_err());
    }
    #[test]
    fn rejects_fake_server_before_sending_input() {
        // A same-user process must not be able to collect password keystrokes
        // by taking the broker's pipe name before the real service starts.
        let pipe = unsafe {
            CreateNamedPipeW(
                wide(&pipe_name(session_id().unwrap())).as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                32768,
                32768,
                0,
                std::ptr::null(),
            )
        };
        assert_ne!(
            pipe, INVALID_HANDLE_VALUE,
            "test requires no installed broker in this session"
        );
        let file = unsafe { file_from_handle(pipe) };
        let server = std::thread::spawn(move || {
            let connected = unsafe { ConnectNamedPipe(file.as_raw_handle(), std::ptr::null_mut()) };
            let error = unsafe { GetLastError() };
            // The rejected client may already have closed before this thread
            // enters ConnectNamedPipe; ERROR_NO_DATA is that documented race.
            assert!(connected != 0 || error == ERROR_PIPE_CONNECTED || error == ERROR_NO_DATA);
            let mut available = 0;
            unsafe {
                PeekNamedPipe(
                    file.as_raw_handle(),
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut available,
                    std::ptr::null_mut(),
                );
            }
            assert_eq!(available, 0, "no keystrokes may reach an untrusted pipe");
        });
        assert!(Connection::connect().is_err());
        server.join().unwrap();
    }
}
