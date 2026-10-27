//! Keys, named by their W3C `KeyboardEvent.code` (the physical key, not the
//! character it types), with each platform's number for the same key.
//!
//! Clients turn their own key numbers into codes; servers turn codes into
//! theirs. A few keys that no standard names, such as Mission Control, have
//! codes of Portputer's own.

use serde::{Deserialize, Serialize};

/// How a Mac produces a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacKey {
    /// A virtual key code (`kVK_*`), posted as a key event.
    Code(u16),
    /// An auxiliary-control key (`NX_KEYTYPE_*`), posted as a system-defined
    /// event, as Apple keyboards send media and brightness keys.
    System(u8),
    None,
}

/// How Windows produces a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WinKey {
    /// A set 1 scan code; 0xE0xx means the extended form.
    Scan(u16),
    /// A virtual key, for keys whose scan code Windows ignores in SendInput.
    Vk(u16),
    None,
}

#[derive(Clone, Copy, Debug)]
pub struct KeyDef {
    pub code: &'static str,
    pub mac: MacKey,
    pub win: WinKey,
    /// Linux input event code, which Android reports as the scan code.
    pub evdev: u16,
    /// Android `KeyEvent.KEYCODE_*`, for key events without a scan code.
    pub android: u16,
}

use MacKey::{Code as M, None as MN, System as MS};
use WinKey::{None as WN, Scan as W, Vk as WV};

const fn k(code: &'static str, mac: MacKey, win: WinKey, evdev: u16, android: u16) -> KeyDef {
    KeyDef { code, mac, win, evdev, android }
}

pub static KEYS: &[KeyDef] = &[
    k("KeyA", M(0x00), W(0x1E), 30, 29),
    k("KeyB", M(0x0B), W(0x30), 48, 30),
    k("KeyC", M(0x08), W(0x2E), 46, 31),
    k("KeyD", M(0x02), W(0x20), 32, 32),
    k("KeyE", M(0x0E), W(0x12), 18, 33),
    k("KeyF", M(0x03), W(0x21), 33, 34),
    k("KeyG", M(0x05), W(0x22), 34, 35),
    k("KeyH", M(0x04), W(0x23), 35, 36),
    k("KeyI", M(0x22), W(0x17), 23, 37),
    k("KeyJ", M(0x26), W(0x24), 36, 38),
    k("KeyK", M(0x28), W(0x25), 37, 39),
    k("KeyL", M(0x25), W(0x26), 38, 40),
    k("KeyM", M(0x2E), W(0x32), 50, 41),
    k("KeyN", M(0x2D), W(0x31), 49, 42),
    k("KeyO", M(0x1F), W(0x18), 24, 43),
    k("KeyP", M(0x23), W(0x19), 25, 44),
    k("KeyQ", M(0x0C), W(0x10), 16, 45),
    k("KeyR", M(0x0F), W(0x13), 19, 46),
    k("KeyS", M(0x01), W(0x1F), 31, 47),
    k("KeyT", M(0x11), W(0x14), 20, 48),
    k("KeyU", M(0x20), W(0x16), 22, 49),
    k("KeyV", M(0x09), W(0x2F), 47, 50),
    k("KeyW", M(0x0D), W(0x11), 17, 51),
    k("KeyX", M(0x07), W(0x2D), 45, 52),
    k("KeyY", M(0x10), W(0x15), 21, 53),
    k("KeyZ", M(0x06), W(0x2C), 44, 54),
    k("Digit1", M(0x12), W(0x02), 2, 8),
    k("Digit2", M(0x13), W(0x03), 3, 9),
    k("Digit3", M(0x14), W(0x04), 4, 10),
    k("Digit4", M(0x15), W(0x05), 5, 11),
    k("Digit5", M(0x17), W(0x06), 6, 12),
    k("Digit6", M(0x16), W(0x07), 7, 13),
    k("Digit7", M(0x1A), W(0x08), 8, 14),
    k("Digit8", M(0x1C), W(0x09), 9, 15),
    k("Digit9", M(0x19), W(0x0A), 10, 16),
    k("Digit0", M(0x1D), W(0x0B), 11, 7),
    k("Enter", M(0x24), W(0x1C), 28, 66),
    k("Escape", M(0x35), W(0x01), 1, 111),
    k("Backspace", M(0x33), W(0x0E), 14, 67),
    k("Tab", M(0x30), W(0x0F), 15, 61),
    k("Space", M(0x31), W(0x39), 57, 62),
    k("Minus", M(0x1B), W(0x0C), 12, 69),
    k("Equal", M(0x18), W(0x0D), 13, 70),
    k("BracketLeft", M(0x21), W(0x1A), 26, 71),
    k("BracketRight", M(0x1E), W(0x1B), 27, 72),
    k("Backslash", M(0x2A), W(0x2B), 43, 73),
    k("Semicolon", M(0x29), W(0x27), 39, 74),
    k("Quote", M(0x27), W(0x28), 40, 75),
    k("Backquote", M(0x32), W(0x29), 41, 68),
    k("Comma", M(0x2B), W(0x33), 51, 55),
    k("Period", M(0x2F), W(0x34), 52, 56),
    k("Slash", M(0x2C), W(0x35), 53, 76),
    k("IntlBackslash", M(0x0A), W(0x56), 86, 0),
    k("IntlRo", M(0x5E), W(0x73), 89, 217),
    k("IntlYen", M(0x5D), W(0x7D), 124, 216),
    k("KanaMode", M(0x68), W(0x70), 90, 218),
    k("Lang1", M(0x68), W(0x72), 122, 0),
    k("Lang2", M(0x66), W(0x71), 123, 212),
    k("Convert", MN, W(0x79), 92, 214),
    k("NonConvert", MN, W(0x7B), 94, 213),
    k("CapsLock", M(0x39), W(0x3A), 58, 115),
    k("F1", M(0x7A), W(0x3B), 59, 131),
    k("F2", M(0x78), W(0x3C), 60, 132),
    k("F3", M(0x63), W(0x3D), 61, 133),
    k("F4", M(0x76), W(0x3E), 62, 134),
    k("F5", M(0x60), W(0x3F), 63, 135),
    k("F6", M(0x61), W(0x40), 64, 136),
    k("F7", M(0x62), W(0x41), 65, 137),
    k("F8", M(0x64), W(0x42), 66, 138),
    k("F9", M(0x65), W(0x43), 67, 139),
    k("F10", M(0x6D), W(0x44), 68, 140),
    k("F11", M(0x67), W(0x57), 87, 141),
    k("F12", M(0x6F), W(0x58), 88, 142),
    k("F13", M(0x69), W(0x64), 183, 0),
    k("F14", M(0x6B), W(0x65), 184, 0),
    k("F15", M(0x71), W(0x66), 185, 0),
    k("F16", M(0x6A), W(0x67), 186, 0),
    k("F17", M(0x40), W(0x68), 187, 0),
    k("F18", M(0x4F), W(0x69), 188, 0),
    k("F19", M(0x50), W(0x6A), 189, 0),
    k("F20", M(0x5A), W(0x6B), 190, 0),
    k("F21", MN, W(0x6C), 191, 0),
    k("F22", MN, W(0x6D), 192, 0),
    k("F23", MN, W(0x6E), 193, 0),
    k("F24", MN, W(0x76), 194, 0),
    // A Mac has no Print Screen, Scroll Lock or Pause: Apple's own PC-layout
    // keyboards put F13, F14 and F15 there.
    k("PrintScreen", M(0x69), WV(0x2C), 99, 120),
    k("ScrollLock", M(0x6B), W(0x46), 70, 116),
    k("Pause", M(0x71), WV(0x13), 119, 121),
    // The Mac key in the Insert position is Help.
    k("Insert", M(0x72), W(0xE052), 110, 124),
    k("Home", M(0x73), W(0xE047), 102, 122),
    k("PageUp", M(0x74), W(0xE049), 104, 92),
    k("Delete", M(0x75), W(0xE053), 111, 112),
    k("End", M(0x77), W(0xE04F), 107, 123),
    k("PageDown", M(0x79), W(0xE051), 109, 93),
    k("ArrowRight", M(0x7C), W(0xE04D), 106, 22),
    k("ArrowLeft", M(0x7B), W(0xE04B), 105, 21),
    k("ArrowDown", M(0x7D), W(0xE050), 108, 20),
    k("ArrowUp", M(0x7E), W(0xE048), 103, 19),
    k("NumLock", M(0x47), WV(0x90), 69, 143),
    k("NumpadDivide", M(0x4B), W(0xE035), 98, 154),
    k("NumpadMultiply", M(0x43), W(0x37), 55, 155),
    k("NumpadSubtract", M(0x4E), W(0x4A), 74, 156),
    k("NumpadAdd", M(0x45), W(0x4E), 78, 157),
    k("NumpadEnter", M(0x4C), W(0xE01C), 96, 160),
    k("Numpad1", M(0x53), W(0x4F), 79, 145),
    k("Numpad2", M(0x54), W(0x50), 80, 146),
    k("Numpad3", M(0x55), W(0x51), 81, 147),
    k("Numpad4", M(0x56), W(0x4B), 75, 148),
    k("Numpad5", M(0x57), W(0x4C), 76, 149),
    k("Numpad6", M(0x58), W(0x4D), 77, 150),
    k("Numpad7", M(0x59), W(0x47), 71, 151),
    k("Numpad8", M(0x5B), W(0x48), 72, 152),
    k("Numpad9", M(0x5C), W(0x49), 73, 153),
    k("Numpad0", M(0x52), W(0x52), 82, 144),
    k("NumpadDecimal", M(0x41), W(0x53), 83, 158),
    k("NumpadEqual", M(0x51), W(0x59), 117, 161),
    k("NumpadComma", M(0x5F), W(0x7E), 121, 159),
    k("ContextMenu", M(0x6E), W(0xE05D), 127, 82),
    k("Help", M(0x72), WN, 138, 259),
    k("ControlLeft", M(0x3B), W(0x1D), 29, 113),
    k("ShiftLeft", M(0x38), W(0x2A), 42, 59),
    k("AltLeft", M(0x3A), W(0x38), 56, 57),
    k("MetaLeft", M(0x37), W(0xE05B), 125, 117),
    k("ControlRight", M(0x3E), W(0xE01D), 97, 114),
    k("ShiftRight", M(0x3C), W(0x36), 54, 60),
    k("AltRight", M(0x3D), W(0xE038), 100, 58),
    k("MetaRight", M(0x36), W(0xE05C), 126, 118),
    k("Fn", M(0x3F), WN, 464, 119),
    k("AudioVolumeMute", MS(7), WV(0xAD), 113, 164),
    k("AudioVolumeDown", MS(1), WV(0xAE), 114, 25),
    k("AudioVolumeUp", MS(0), WV(0xAF), 115, 24),
    k("MediaPlayPause", MS(16), WV(0xB3), 164, 85),
    k("MediaStop", MN, WV(0xB2), 166, 86),
    k("MediaTrackNext", MS(17), WV(0xB0), 163, 87),
    k("MediaTrackPrevious", MS(18), WV(0xB1), 165, 88),
    k("MediaFastForward", MS(19), WN, 208, 90),
    k("MediaRewind", MS(20), WN, 168, 89),
    k("BrightnessUp", MS(2), WN, 225, 221),
    k("BrightnessDown", MS(3), WN, 224, 220),
    k("KeyboardBacklightUp", MS(21), WN, 230, 0),
    k("KeyboardBacklightDown", MS(22), WN, 229, 0),
    k("Eject", MS(14), WN, 161, 129),
    k("MissionControl", M(0xA0), WN, 120, 0),
    k("Launchpad", M(0x83), WN, 0, 0),
    k("BrowserSearch", MN, WV(0xAA), 217, 0),
    k("BrowserHome", MN, WV(0xAC), 172, 0),
    k("BrowserBack", MN, WV(0xA6), 158, 0),
    k("BrowserForward", MN, WV(0xA7), 159, 0),
    k("BrowserRefresh", MN, WV(0xA8), 173, 0),
    k("LaunchMail", MN, WV(0xB4), 155, 0),
    k("Sleep", MN, WV(0x5F), 142, 223),
];

pub fn by_code(code: &str) -> Option<&'static KeyDef> {
    KEYS.iter().find(|k| k.code == code)
}

/// The code of a key from its Linux input event code (Android's scan code).
pub fn from_evdev(scan: u16) -> Option<&'static str> {
    if scan == 0 {
        return None;
    }
    KEYS.iter().find(|k| k.evdev == scan).map(|k| k.code)
}

/// The code of a key from Android's key code, for events with no scan code.
pub fn from_android(keycode: u16) -> Option<&'static str> {
    if keycode == 0 {
        return None;
    }
    KEYS.iter().find(|k| k.android == keycode).map(|k| k.code)
}

pub fn from_mac(code: u16) -> Option<&'static str> {
    KEYS.iter().find(|k| k.mac == MacKey::Code(code)).map(|k| k.code)
}

/// The code of a key from its Windows scan code (0xE0xx for extended keys).
pub fn from_windows_scan(scan: u16) -> Option<&'static str> {
    KEYS.iter().find(|k| k.win == WinKey::Scan(scan)).map(|k| k.code)
}

pub fn from_windows_vk(vk: u16) -> Option<&'static str> {
    KEYS.iter().find(|k| k.win == WinKey::Vk(vk)).map(|k| k.code)
}

pub fn is_modifier(code: &str) -> bool {
    matches!(
        code,
        "ControlLeft" | "ControlRight" | "ShiftLeft" | "ShiftRight" | "AltLeft" | "AltRight"
            | "MetaLeft" | "MetaRight" | "Fn" | "CapsLock"
    )
}

/// The operating systems a server can run on, which decide key names and the
/// key menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Mac,
    Windows,
    Other,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(target_os = "macos") {
            Os::Mac
        } else if cfg!(windows) {
            Os::Windows
        } else {
            Os::Other
        }
    }
}

/// A key's name as printed on keyboards for that system.
pub fn label(code: &str, os: Os) -> String {
    let mac = os == Os::Mac;
    let fixed = match code {
        "Enter" => if mac { "Return" } else { "Enter" },
        "Escape" => "Escape",
        "Backspace" => if mac { "Delete" } else { "Backspace" },
        "Delete" => if mac { "Forward Delete" } else { "Delete" },
        "Tab" => "Tab",
        "Space" => "Space",
        "Minus" => "Minus",
        "Equal" => "Equals",
        "BracketLeft" => "Left Bracket",
        "BracketRight" => "Right Bracket",
        "Backslash" => "Backslash",
        "Semicolon" => "Semicolon",
        "Quote" => "Apostrophe",
        "Backquote" => "Grave Accent",
        "Comma" => "Comma",
        "Period" => "Period",
        "Slash" => "Slash",
        "IntlBackslash" => if mac { "Section" } else { "Backslash (ISO)" },
        "IntlRo" => "Ro",
        "IntlYen" => "Yen",
        "KanaMode" => "Kana",
        "Lang1" => if mac { "Kana" } else { "Han/Yeong" },
        "Lang2" => if mac { "Eisu" } else { "Hanja" },
        "Convert" => "Convert",
        "NonConvert" => "Non-convert",
        "CapsLock" => "Caps Lock",
        "PrintScreen" => if mac { "F13 (Print Screen)" } else { "Print Screen" },
        "ScrollLock" => if mac { "F14 (Scroll Lock)" } else { "Scroll Lock" },
        "Pause" => if mac { "F15 (Pause)" } else { "Pause" },
        "Insert" => if mac { "Help (Insert)" } else { "Insert" },
        "Home" => "Home",
        "End" => "End",
        "PageUp" => "Page Up",
        "PageDown" => "Page Down",
        "ArrowRight" => "Right Arrow",
        "ArrowLeft" => "Left Arrow",
        "ArrowDown" => "Down Arrow",
        "ArrowUp" => "Up Arrow",
        "NumLock" => if mac { "Clear" } else { "Num Lock" },
        "NumpadDivide" => "Keypad Divide",
        "NumpadMultiply" => "Keypad Multiply",
        "NumpadSubtract" => "Keypad Minus",
        "NumpadAdd" => "Keypad Plus",
        "NumpadEnter" => "Keypad Enter",
        "NumpadDecimal" => "Keypad Decimal",
        "NumpadEqual" => "Keypad Equals",
        "NumpadComma" => "Keypad Comma",
        "ContextMenu" => if mac { "Contextual Menu" } else { "Applications" },
        "Help" => "Help",
        "ControlLeft" => "Left Control",
        "ControlRight" => "Right Control",
        "ShiftLeft" => "Left Shift",
        "ShiftRight" => "Right Shift",
        "AltLeft" => if mac { "Left Option" } else { "Left Alt" },
        "AltRight" => if mac { "Right Option" } else { "Right Alt" },
        "MetaLeft" => if mac { "Left Command" } else { "Left Windows" },
        "MetaRight" => if mac { "Right Command" } else { "Right Windows" },
        "Fn" => if mac { "Fn (Globe)" } else { "Fn" },
        "AudioVolumeMute" => "Mute",
        "AudioVolumeDown" => "Volume Down",
        "AudioVolumeUp" => "Volume Up",
        "MediaPlayPause" => "Play/Pause",
        "MediaStop" => "Stop",
        "MediaTrackNext" => "Next Track",
        "MediaTrackPrevious" => "Previous Track",
        "MediaFastForward" => "Fast Forward",
        "MediaRewind" => "Rewind",
        "BrightnessUp" => "Brightness Up",
        "BrightnessDown" => "Brightness Down",
        "KeyboardBacklightUp" => "Keyboard Brightness Up",
        "KeyboardBacklightDown" => "Keyboard Brightness Down",
        "Eject" => "Eject",
        "MissionControl" => "Mission Control",
        "Launchpad" => "Launchpad",
        "BrowserSearch" => "Browser Search",
        "BrowserHome" => "Browser Home",
        "BrowserBack" => "Browser Back",
        "BrowserForward" => "Browser Forward",
        "BrowserRefresh" => "Browser Refresh",
        "LaunchMail" => "Mail",
        "Sleep" => "Sleep",
        _ => "",
    };
    if !fixed.is_empty() {
        return fixed.to_string();
    }
    if let Some(letter) = code.strip_prefix("Key") {
        return letter.to_string();
    }
    if let Some(digit) = code.strip_prefix("Digit") {
        return digit.to_string();
    }
    if let Some(digit) = code.strip_prefix("Numpad") {
        return format!("Keypad {digit}");
    }
    code.to_string()
}

/// Whether the server's system can produce the key.
pub fn supported(code: &str, os: Os) -> bool {
    match (by_code(code), os) {
        (Some(def), Os::Mac) => def.mac != MacKey::None,
        (Some(def), Os::Windows) => def.win != WinKey::None,
        (Some(_), Os::Other) => true,
        (None, _) => false,
    }
}

/// One entry of the key menu: a key, with any modifiers held while it's
/// pressed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuKey {
    pub label: String,
    pub code: String,
    pub modifiers: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct MenuSection {
    pub title: String,
    pub keys: Vec<MenuKey>,
}

/// The keys offered in the client's key menu for a server's system: keys
/// that phone and laptop keyboards often lack, then common combinations.
pub fn menu(os: Os) -> Vec<MenuSection> {
    let single = |codes: &[&str]| -> Vec<MenuKey> {
        codes
            .iter()
            .filter(|c| supported(c, os))
            .map(|c| MenuKey { label: label(c, os), code: c.to_string(), modifiers: vec![] })
            .collect()
    };
    let combo = |label: &str, mods: &[&str], code: &str| MenuKey {
        label: label.to_string(),
        code: code.to_string(),
        modifiers: mods.iter().map(|m| m.to_string()).collect(),
    };
    let function: Vec<String> = (1..=24).map(|n| format!("F{n}")).collect();
    let function: Vec<&str> = function.iter().map(String::as_str).collect();
    let mut sections = vec![
        MenuSection {
            title: "Modifiers".into(),
            keys: single(&[
                "MetaLeft", "AltLeft", "ControlLeft", "ShiftLeft", "Fn", "MetaRight", "AltRight",
                "ControlRight", "ShiftRight", "CapsLock",
            ]),
        },
        MenuSection { title: "Function Keys".into(), keys: single(&function) },
        MenuSection {
            title: "Media".into(),
            keys: single(&[
                "AudioVolumeUp", "AudioVolumeDown", "AudioVolumeMute", "MediaPlayPause",
                "MediaTrackNext", "MediaTrackPrevious", "MediaFastForward", "MediaRewind",
                "MediaStop",
            ]),
        },
        MenuSection {
            title: "System".into(),
            keys: single(&[
                "BrightnessUp", "BrightnessDown", "KeyboardBacklightUp", "KeyboardBacklightDown",
                "MissionControl", "Launchpad", "Eject", "Sleep", "BrowserSearch", "LaunchMail",
            ]),
        },
        MenuSection {
            title: "Navigation and Editing".into(),
            keys: single(&[
                "Escape", "Tab", "Delete", "Insert", "Help", "Home", "End", "PageUp", "PageDown",
                "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "ContextMenu", "PrintScreen",
                "ScrollLock", "Pause", "NumLock", "NumpadEnter", "NumpadEqual", "NumpadComma",
            ]),
        },
    ];
    let combos = match os {
        Os::Mac => vec![
            combo("VoiceOver On or Off (Command-F5)", &["MetaLeft"], "F5"),
            combo("Spotlight (Command-Space)", &["MetaLeft"], "Space"),
            combo("App Switcher (Command-Tab)", &["MetaLeft"], "Tab"),
            combo("Force Quit (Option-Command-Escape)", &["AltLeft", "MetaLeft"], "Escape"),
            combo("Lock Screen (Control-Command-Q)", &["ControlLeft", "MetaLeft"], "KeyQ"),
            combo("Quit App (Command-Q)", &["MetaLeft"], "KeyQ"),
            combo("Close Window (Command-W)", &["MetaLeft"], "KeyW"),
            combo("Move Focus to Menu Bar (Control-F2)", &["ControlLeft"], "F2"),
            combo("Move Focus to Dock (Control-F3)", &["ControlLeft"], "F3"),
            combo("Screenshot (Shift-Command-3)", &["ShiftLeft", "MetaLeft"], "Digit3"),
            combo("Screenshot Tools (Shift-Command-5)", &["ShiftLeft", "MetaLeft"], "Digit5"),
            combo("Accessibility Shortcuts (Option-Command-F5)", &["AltLeft", "MetaLeft"], "F5"),
            combo("Notification Center (Fn-N)", &["Fn"], "KeyN"),
            combo("Control Center (Fn-C)", &["Fn"], "KeyC"),
            combo("Dictation (Fn-D)", &["Fn"], "KeyD"),
            combo("Emoji and Symbols (Fn-E)", &["Fn"], "KeyE"),
        ],
        Os::Windows => vec![
            combo("Secure Attention (Control-Alt-Delete)", &["ControlLeft", "AltLeft"], "Delete"),
            combo("Start Menu (Windows)", &[], "MetaLeft"),
            combo("Lock (Windows-L)", &["MetaLeft"], "KeyL"),
            combo("Show Desktop (Windows-D)", &["MetaLeft"], "KeyD"),
            combo("Run (Windows-R)", &["MetaLeft"], "KeyR"),
            combo("Quick Link Menu (Windows-X)", &["MetaLeft"], "KeyX"),
            combo("File Explorer (Windows-E)", &["MetaLeft"], "KeyE"),
            combo("Settings (Windows-I)", &["MetaLeft"], "KeyI"),
            combo("Notifications (Windows-N)", &["MetaLeft"], "KeyN"),
            combo("Narrator (Control-Windows-Enter)", &["ControlLeft", "MetaLeft"], "Enter"),
            combo("Switch Apps (Alt-Tab)", &["AltLeft"], "Tab"),
            combo("Close App (Alt-F4)", &["AltLeft"], "F4"),
            combo("Task Manager (Control-Shift-Escape)", &["ControlLeft", "ShiftLeft"], "Escape"),
            combo("System Tray (Windows-B)", &["MetaLeft"], "KeyB"),
            combo("Screenshot (Windows-Shift-S)", &["MetaLeft", "ShiftLeft"], "KeyS"),
        ],
        Os::Other => vec![],
    };
    if !combos.is_empty() {
        sections.insert(0, MenuSection { title: "Shortcuts".into(), keys: combos });
    }
    sections.retain(|s| !s.keys.is_empty());
    sections
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique() {
        for (i, a) in KEYS.iter().enumerate() {
            for b in &KEYS[i + 1..] {
                assert_ne!(a.code, b.code);
            }
        }
    }

    #[test]
    fn evdev_codes_are_unique() {
        for (i, a) in KEYS.iter().enumerate() {
            for b in &KEYS[i + 1..] {
                if a.evdev != 0 {
                    assert_ne!(a.evdev, b.evdev, "{} and {}", a.code, b.code);
                }
            }
        }
    }

    #[test]
    fn android_mappings() {
        assert_eq!(from_evdev(30), Some("KeyA"));
        assert_eq!(from_evdev(125), Some("MetaLeft"));
        assert_eq!(from_android(67), Some("Backspace"));
        assert_eq!(from_android(112), Some("Delete"));
        assert_eq!(from_evdev(0), None);
    }

    #[test]
    fn menu_keys_are_known() {
        for os in [Os::Mac, Os::Windows] {
            for section in menu(os) {
                for key in section.keys {
                    assert!(by_code(&key.code).is_some(), "{}", key.code);
                    for m in key.modifiers {
                        assert!(is_modifier(&m), "{m}");
                    }
                }
            }
        }
    }

    #[test]
    fn labels() {
        assert_eq!(label("MetaLeft", Os::Mac), "Left Command");
        assert_eq!(label("KeyQ", Os::Windows), "Q");
        assert_eq!(label("Numpad7", Os::Mac), "Keypad 7");
    }
}
