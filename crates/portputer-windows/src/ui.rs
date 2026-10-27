//! A thin layer over standard Win32 controls: creating, reading and setting
//! them, laying them out top to bottom, and running work on the window
//! thread.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontIndirectW, HFONT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::PCWSTR;

pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

pub fn hinstance() -> HINSTANCE {
    unsafe { GetModuleHandleW(None).unwrap_or_default().into() }
}

/// The message that runs queued work on the window thread.
pub const WM_RUN: u32 = WM_APP + 1;
/// The notification icon's message.
pub const WM_TRAY: u32 = WM_APP + 2;

static MAIN_WINDOW: AtomicIsize = AtomicIsize::new(0);
type Work = Box<dyn FnOnce() + Send>;
static QUEUE: Mutex<VecDeque<Work>> = Mutex::new(VecDeque::new());

pub fn set_main_window(hwnd: HWND) {
    MAIN_WINDOW.store(hwnd.0 as isize, Ordering::SeqCst);
}

pub fn main_window() -> HWND {
    HWND(MAIN_WINDOW.load(Ordering::SeqCst) as *mut _)
}

/// Runs work on the window thread, from any thread.
pub fn run_on_ui(work: impl FnOnce() + Send + 'static) {
    QUEUE.lock().unwrap().push_back(Box::new(work));
    unsafe {
        let _ = PostMessageW(Some(main_window()), WM_RUN, WPARAM(0), LPARAM(0));
    }
}

pub fn run_queued() {
    loop {
        let next = QUEUE.lock().unwrap().pop_front();
        match next {
            Some(work) => work(),
            None => break,
        }
    }
}

/// Windows' message font, as dialogs use.
pub fn message_font() -> HFONT {
    unsafe {
        let mut metrics = NONCLIENTMETRICSW { cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32, ..Default::default() };
        let _ = SystemParametersInfoW(
            SPI_GETNONCLIENTMETRICS,
            metrics.cbSize,
            Some(&mut metrics as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
        CreateFontIndirectW(&metrics.lfMessageFont)
    }
}

pub fn create(parent: HWND, class: &str, text: &str, style: WINDOW_STYLE, ex: WINDOW_EX_STYLE, id: u16) -> HWND {
    let class = wide(class);
    let text = wide(text);
    unsafe {
        let hwnd = CreateWindowExW(
            ex,
            PCWSTR(class.as_ptr()),
            PCWSTR(text.as_ptr()),
            WS_CHILD | WS_VISIBLE | style,
            0,
            0,
            10,
            10,
            Some(parent),
            Some(HMENU(id as usize as *mut _)),
            Some(hinstance()),
            None,
        )
        .unwrap_or_default();
        SendMessageW(hwnd, WM_SETFONT, Some(WPARAM(FONT.with(|f| f.0 as usize))), Some(LPARAM(1)));
        hwnd
    }
}

thread_local! {
    pub static FONT: HFONT = message_font();
}

pub fn label(parent: HWND, text: &str) -> HWND {
    create(parent, "STATIC", text, WINDOW_STYLE(0), WINDOW_EX_STYLE(0), 0)
}

pub fn button(parent: HWND, text: &str, id: u16) -> HWND {
    create(parent, "BUTTON", text, WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32), WINDOW_EX_STYLE(0), id)
}

pub fn checkbox(parent: HWND, text: &str, id: u16) -> HWND {
    create(parent, "BUTTON", text, WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32), WINDOW_EX_STYLE(0), id)
}

pub fn edit(parent: HWND, id: u16, read_only: bool) -> HWND {
    let mut style = WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32);
    if read_only {
        style |= WINDOW_STYLE(ES_READONLY as u32);
    }
    create(parent, "EDIT", "", style, WS_EX_CLIENTEDGE, id)
}

pub fn listbox(parent: HWND, id: u16) -> HWND {
    create(
        parent,
        "LISTBOX",
        "",
        WS_TABSTOP | WS_VSCROLL | WINDOW_STYLE((LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32),
        WS_EX_CLIENTEDGE,
        id,
    )
}

// Trackbar messages, from commctrl.h.
const TBM_GETPOS: u32 = WM_USER;
const TBM_SETPOS: u32 = WM_USER + 5;
const TBM_SETRANGE: u32 = WM_USER + 6;
const TBM_SETPAGESIZE: u32 = WM_USER + 21;

pub fn trackbar(parent: HWND, id: u16) -> HWND {
    let hwnd = create(parent, "msctls_trackbar32", "", WS_TABSTOP, WINDOW_EX_STYLE(0), id);
    unsafe {
        SendMessageW(hwnd, TBM_SETRANGE, Some(WPARAM(1)), Some(LPARAM((100 << 16) as isize)));
        SendMessageW(hwnd, TBM_SETPAGESIZE, Some(WPARAM(0)), Some(LPARAM(10)));
    }
    hwnd
}

pub fn set_text(hwnd: HWND, text: &str) {
    let w = wide(text);
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(w.as_ptr()));
    }
}

pub fn text(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

pub fn checked(hwnd: HWND) -> bool {
    unsafe { SendMessageW(hwnd, BM_GETCHECK, None, None).0 == 1 }
}

pub fn set_checked(hwnd: HWND, on: bool) {
    unsafe {
        SendMessageW(hwnd, BM_SETCHECK, Some(WPARAM(on as usize)), None);
    }
}

pub fn show(hwnd: HWND, visible: bool) {
    unsafe {
        let _ = ShowWindow(hwnd, if visible { SW_SHOW } else { SW_HIDE });
    }
}

pub fn enable(hwnd: HWND, enabled: bool) {
    unsafe {
        let _ = EnableWindow(hwnd, enabled);
    }
}

pub fn visible(hwnd: HWND) -> bool {
    unsafe { IsWindowVisible(hwnd).as_bool() }
}

pub fn list_set(hwnd: HWND, items: &[String]) {
    unsafe {
        let selected = SendMessageW(hwnd, LB_GETCURSEL, None, None).0;
        SendMessageW(hwnd, LB_RESETCONTENT, None, None);
        for item in items {
            let w = wide(item);
            SendMessageW(hwnd, LB_ADDSTRING, None, Some(LPARAM(w.as_ptr() as isize)));
        }
        let keep = if selected >= 0 && (selected as usize) < items.len() { selected } else { 0 };
        if !items.is_empty() {
            SendMessageW(hwnd, LB_SETCURSEL, Some(WPARAM(keep as usize)), None);
        }
    }
}

pub fn list_selection(hwnd: HWND) -> Option<usize> {
    let i = unsafe { SendMessageW(hwnd, LB_GETCURSEL, None, None).0 };
    (i >= 0).then_some(i as usize)
}

pub fn trackbar_pos(hwnd: HWND) -> u32 {
    unsafe { SendMessageW(hwnd, TBM_GETPOS, None, None).0 as u32 }
}

pub fn set_trackbar_pos(hwnd: HWND, pos: u32) {
    unsafe {
        SendMessageW(hwnd, TBM_SETPOS, Some(WPARAM(1)), Some(LPARAM(pos as isize)));
    }
}

/// Lays visible controls out top to bottom: each row is one or more
/// controls side by side, with their widths as shares of the row.
pub struct Flow {
    pub x: i32,
    pub y: i32,
    pub width: i32,
}

impl Flow {
    pub fn row(&mut self, controls: &[(HWND, i32)], height: i32) {
        let visible: Vec<&(HWND, i32)> = controls.iter().filter(|(h, _)| visible(*h)).collect();
        if visible.is_empty() {
            return;
        }
        let total: i32 = visible.iter().map(|(_, share)| *share).sum();
        let gap = 8;
        let usable = self.width - gap * (visible.len() as i32 - 1);
        let mut x = self.x;
        for (hwnd, share) in visible {
            let w = usable * share / total.max(1);
            unsafe {
                let _ = MoveWindow(*hwnd, x, self.y, w, height, true);
            }
            x += w + gap;
        }
        self.y += height + 6;
    }

    pub fn gap(&mut self, size: i32) {
        self.y += size;
    }
}

/// Shows a message box; true for OK or Yes.
pub fn confirm(owner: HWND, title: &str, text: &str) -> bool {
    let t = wide(title);
    let m = wide(text);
    unsafe { MessageBoxW(Some(owner), PCWSTR(m.as_ptr()), PCWSTR(t.as_ptr()), MB_OKCANCEL | MB_ICONQUESTION) == IDOK }
}
