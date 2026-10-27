//! Telling the screen reader about changes: through NVDA's controller client
//! (nvdaControllerClient.dll beside Portputer.exe) when NVDA runs, and as a
//! UI Automation live region on the status text for the others.

use std::sync::OnceLock;

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{CAccPropServices, IAccPropServices, LiveSetting_Property_GUID, NotifyWinEvent, Polite};
use windows::Win32::UI::WindowsAndMessaging::{CHILDID_SELF, EVENT_OBJECT_LIVEREGIONCHANGED, OBJID_CLIENT};
use windows::core::{PCWSTR, s};

use crate::ui;

type NoArguments = unsafe extern "system" fn() -> u32;
type WithText = unsafe extern "system" fn(PCWSTR) -> u32;

struct Nvda {
    test_if_running: NoArguments,
    speak_text: WithText,
}

fn nvda() -> Option<&'static Nvda> {
    static CLIENT: OnceLock<Option<Nvda>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            let path = std::env::current_exe().ok()?.parent()?.join("nvdaControllerClient.dll");
            let path = ui::wide(&path.to_string_lossy());
            let module = unsafe { LoadLibraryW(PCWSTR(path.as_ptr())) }.ok()?;
            type Found = unsafe extern "system" fn() -> isize;
            let find = |name| unsafe { GetProcAddress(module, name) };
            Some(Nvda {
                test_if_running: unsafe { std::mem::transmute::<Found, NoArguments>(find(s!("nvdaController_testIfRunning"))?) },
                speak_text: unsafe { std::mem::transmute::<Found, WithText>(find(s!("nvdaController_speakText"))?) },
            })
        })
        .as_ref()
}

/// Speaks through NVDA if it's running. False if it isn't.
pub fn speak(text: &str) -> bool {
    let Some(client) = nvda() else { return false };
    unsafe {
        if (client.test_if_running)() != 0 {
            return false;
        }
        let w = ui::wide(text);
        (client.speak_text)(PCWSTR(w.as_ptr())) == 0
    }
}

/// Marks a static control as a polite live region.
pub fn make_live_region(hwnd: HWND) {
    unsafe {
        let Ok(services) = CoCreateInstance::<_, IAccPropServices>(&CAccPropServices, None, CLSCTX_INPROC_SERVER) else { return };
        let value = VARIANT::from(Polite.0);
        let _ = services.SetHwndProp(hwnd, OBJID_CLIENT.0 as u32, CHILDID_SELF, LiveSetting_Property_GUID, &value);
    }
}

/// Sets a live region's text and announces it: by NVDA directly, or by the
/// live region event for other screen readers.
pub fn status(hwnd: HWND, text: &str) {
    ui::set_text(hwnd, text);
    if !speak(text) {
        unsafe { NotifyWinEvent(EVENT_OBJECT_LIVEREGIONCHANGED, hwnd, OBJID_CLIENT.0, CHILDID_SELF as i32) };
    }
}
