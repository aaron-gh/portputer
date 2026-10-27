//! The computer's clipboard: text, or files copied in the Finder or File
//! Explorer.

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub enum Clip {
    Text(String),
    Files(Vec<PathBuf>),
}

#[cfg(target_os = "macos")]
mod imp {
    use std::path::{Path, PathBuf};

    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeFileURL, NSPasteboardTypeString, NSPasteboardWriting};
    use objc2_foundation::{NSArray, NSString, NSURL};

    use super::Clip;

    pub fn change_count() -> i64 {
        NSPasteboard::generalPasteboard().changeCount() as i64
    }

    pub fn read() -> Option<Clip> {
        let pasteboard = NSPasteboard::generalPasteboard();
        let mut files = vec![];
        if let Some(items) = pasteboard.pasteboardItems() {
            for item in items.iter() {
                let url = item
                    .stringForType(unsafe { NSPasteboardTypeFileURL })
                    .and_then(|s| NSURL::URLWithString(&s))
                    .and_then(|u| u.path());
                if let Some(path) = url {
                    files.push(PathBuf::from(path.to_string()));
                }
            }
        }
        if !files.is_empty() {
            return Some(Clip::Files(files));
        }
        pasteboard.stringForType(unsafe { NSPasteboardTypeString }).map(|s| Clip::Text(s.to_string()))
    }

    pub fn write_text(text: &str) -> Result<(), String> {
        let pasteboard = NSPasteboard::generalPasteboard();
        pasteboard.clearContents();
        if pasteboard.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString }) {
            Ok(())
        } else {
            Err("the clipboard refused the text".into())
        }
    }

    pub fn write_files(paths: &[&Path]) -> Result<(), String> {
        let pasteboard = NSPasteboard::generalPasteboard();
        pasteboard.clearContents();
        let urls: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = paths
            .iter()
            .map(|p| ProtocolObject::from_retained(NSURL::fileURLWithPath(&NSString::from_str(&p.to_string_lossy()))))
            .collect();
        if pasteboard.writeObjects(&NSArray::from_retained_slice(&urls)) {
            Ok(())
        } else {
            Err("the clipboard refused the files".into())
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::path::{Path, PathBuf};

    use windows::Win32::Foundation::{HANDLE, HGLOBAL};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber, IsClipboardFormatAvailable,
        OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    use windows::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT};
    use windows::Win32::UI::Shell::{DROPFILES, DragQueryFileW, HDROP};

    use super::Clip;

    pub fn change_count() -> i64 {
        unsafe { GetClipboardSequenceNumber() as i64 }
    }

    /// Opens the clipboard, waiting briefly if another program has it.
    fn open() -> Result<Opened, String> {
        for _ in 0..10 {
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Ok(Opened);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        Err("another program is using the clipboard".into())
    }

    struct Opened;

    impl Drop for Opened {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseClipboard();
            }
        }
    }

    pub fn read() -> Option<Clip> {
        let _open = open().ok()?;
        unsafe {
            if IsClipboardFormatAvailable(CF_HDROP.0 as u32).is_ok() {
                let handle = GetClipboardData(CF_HDROP.0 as u32).ok()?;
                let drop = HDROP(handle.0);
                let count = DragQueryFileW(drop, u32::MAX, None);
                let mut files = vec![];
                for i in 0..count {
                    let len = DragQueryFileW(drop, i, None) as usize;
                    let mut buf = vec![0u16; len + 1];
                    let n = DragQueryFileW(drop, i, Some(&mut buf)) as usize;
                    files.push(PathBuf::from(String::from_utf16_lossy(&buf[..n])));
                }
                if !files.is_empty() {
                    return Some(Clip::Files(files));
                }
            }
            if IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).is_ok() {
                let handle = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
                let global = HGLOBAL(handle.0);
                let ptr = GlobalLock(global) as *const u16;
                if ptr.is_null() {
                    return None;
                }
                let mut len = 0;
                while *ptr.add(len) != 0 {
                    len += 1;
                }
                let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
                let _ = GlobalUnlock(global);
                return Some(Clip::Text(text));
            }
        }
        None
    }

    fn set(format: u32, bytes: &[u8]) -> Result<(), String> {
        let _open = open()?;
        unsafe {
            let global = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).map_err(|e| e.to_string())?;
            let ptr = GlobalLock(global) as *mut u8;
            if ptr.is_null() {
                return Err("GlobalLock failed".into());
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
            let _ = GlobalUnlock(global);
            EmptyClipboard().map_err(|e| e.to_string())?;
            // The clipboard owns the memory once it's set.
            SetClipboardData(format, Some(HANDLE(global.0))).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn write_text(text: &str) -> Result<(), String> {
        let bytes: Vec<u8> = text.encode_utf16().chain([0]).flat_map(|u| u.to_le_bytes()).collect();
        set(CF_UNICODETEXT.0 as u32, &bytes)
    }

    pub fn write_files(paths: &[&Path]) -> Result<(), String> {
        let header = DROPFILES { pFiles: std::mem::size_of::<DROPFILES>() as u32, fWide: true.into(), ..Default::default() };
        let mut bytes = unsafe {
            std::slice::from_raw_parts(&header as *const DROPFILES as *const u8, std::mem::size_of::<DROPFILES>()).to_vec()
        };
        for path in paths {
            bytes.extend(path.to_string_lossy().encode_utf16().chain([0]).flat_map(|u| u.to_le_bytes()));
        }
        bytes.extend([0, 0]);
        set(CF_HDROP.0 as u32, &bytes)
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    use std::path::Path;

    use super::Clip;

    pub fn change_count() -> i64 {
        0
    }
    pub fn read() -> Option<Clip> {
        None
    }
    pub fn write_text(_text: &str) -> Result<(), String> {
        Err("no clipboard here".into())
    }
    pub fn write_files(_paths: &[&std::path::Path]) -> Result<(), String> {
        let _ = Path::new("");
        Err("no clipboard here".into())
    }
}

/// A number that changes whenever anything is copied.
pub fn change_count() -> i64 {
    imp::change_count()
}

pub fn read() -> Option<Clip> {
    imp::read()
}

pub fn write_text(text: &str) -> Result<(), String> {
    imp::write_text(text)
}

pub fn write_files(paths: &[&std::path::Path]) -> Result<(), String> {
    imp::write_files(paths)
}

/// Whether this system has a clipboard Portputer can use.
pub fn available() -> bool {
    cfg!(any(target_os = "macos", windows))
}
