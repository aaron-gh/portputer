import UIKit

/// Portputer's settings, in UserDefaults.
enum Settings {
    private static let defaults = UserDefaults.standard

    private static func bool(_ key: String, _ fallback: Bool) -> Bool {
        defaults.object(forKey: key) as? Bool ?? fallback
    }

    static var deviceName: String {
        get { defaults.string(forKey: "deviceName").flatMap { $0.isEmpty ? nil : $0 } ?? UIDevice.current.name }
        set { defaults.set(newValue.trimmingCharacters(in: .whitespaces), forKey: "deviceName") }
    }

    /// Keys by position (the HID usage), so the computer's layout applies, or
    /// by this device's layout.
    static var byPosition: Bool {
        get { bool("byPosition", true) }
        set { defaults.set(newValue, forKey: "byPosition") }
    }

    /// Send Keyboard to Computer: kept across connections.
    static var sendKeyboard: Bool {
        get { bool("sendKeyboard", true) }
        set { defaults.set(newValue, forKey: "sendKeyboard") }
    }

    /// Milliseconds; 0 is automatic.
    static var bufferMs: Int {
        get { defaults.integer(forKey: "bufferMs") }
        set { defaults.set(newValue, forKey: "bufferMs") }
    }

    static let buffers = [0, 20, 40, 60, 100, 150, 250]

    static var volume: Double {
        get { defaults.object(forKey: "volume") as? Double ?? 100 }
        set { defaults.set(newValue, forKey: "volume") }
    }

    static var audioOnConnect: Bool {
        get { bool("audioOnConnect", true) }
        set { defaults.set(newValue, forKey: "audioOnConnect") }
    }

    /// The computer's own speakers stay silent while its audio plays here.
    static var muteSpeakers: Bool {
        get { bool("muteSpeakers", false) }
        set { defaults.set(newValue, forKey: "muteSpeakers") }
    }

    static var shareClipboard: Bool {
        get { bool("shareClipboard", true) }
        set { defaults.set(newValue, forKey: "shareClipboard") }
    }

    static var version: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? ""
    }

    /// Where the core keeps this device's identity and paired computers.
    static let storeDir: String = {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        let dir = base.appendingPathComponent("client", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.path
    }()

    /// Downloads, shown in the Files app under On My iPhone, Portputer.
    static var downloadsDir: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
    }
}

func announce(_ text: String) {
    UIAccessibility.post(notification: .announcement, argument: text)
}

func fileCount(_ n: Int) -> String {
    n == 1 ? "1 file" : "\(n) files"
}

func fileSize(_ bytes: UInt64) -> String {
    ByteCountFormatter.string(fromByteCount: Int64(bytes), countStyle: .file)
}
