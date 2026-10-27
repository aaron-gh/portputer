import AppKit
import Foundation
import ServiceManagement

/// The server this Mac runs for paired devices, and its settings.
@MainActor
final class ServerModel: ObservableObject {
    static let shared = ServerModel()

    @Published private(set) var running = false
    @Published private(set) var port: UInt16 = 0
    @Published private(set) var serverId = ""
    @Published private(set) var connected: [ConnectedDevice] = []
    @Published private(set) var paired: [PairedDevice] = []
    @Published private(set) var pairingCode: String?
    @Published private(set) var pairingSecondsLeft: UInt64 = 0
    @Published private(set) var inputAllowed = false
    @Published private(set) var systemComponents = false
    @Published private(set) var microphoneDriver = false
    @Published var relayEnabled = true
    @Published var relayCustomURL = ""
    @Published private(set) var relayBuiltIn = ""
    @Published private(set) var relayStatus = "Connecting."
    @Published private(set) var openAtLogin = false
    @Published var status = ""

    private var handle: ServerHandle?
    private var timer: Timer?

    static var supportDir: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Portputer", isDirectory: true)
    }

    let storeDir = ServerModel.supportDir.appendingPathComponent("server", isDirectory: true).path

    var serverEnabled: Bool {
        get { UserDefaults.standard.object(forKey: "serverEnabled") as? Bool ?? true }
        set {
            UserDefaults.standard.set(newValue, forKey: "serverEnabled")
            newValue ? start() : stop()
        }
    }

    var announceConnections: Bool {
        get { UserDefaults.standard.object(forKey: "announceConnections") as? Bool ?? true }
        set {
            UserDefaults.standard.set(newValue, forKey: "announceConnections")
            objectWillChange.send()
        }
    }

    func launch() {
        // The first start asks for Accessibility permission.
        inputAllowed = Portputer.inputAllowed(ask: true)
        diagnostic("Typing permission at launch: \(inputAllowed ? "allowed" : "not allowed") (\(inputPermissionDetail()))")
        refresh()
        if serverEnabled { start() }
        timer = Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { _ in
            Task { @MainActor in ServerModel.shared.tick() }
        }
    }

    func start() {
        guard handle == nil else { return }
        do {
            handle = try ServerHandle.start(storeDir: storeDir, name: computerName(), port: 47800, listener: Events())
            running = true
            port = handle?.port() ?? 0
            serverId = handle?.id() ?? ""
            status = "Server on, port \(port)."
        } catch {
            running = false
            status = "Server not started: \(error.localizedDescription)"
        }
        refresh()
    }

    func stop() {
        handle?.stop()
        handle = nil
        running = false
        connected = []
        pairingCode = nil
        status = "Server off."
    }

    func refresh() {
        paired = handle?.paired() ?? []
        connected = handle?.connected() ?? []
        if let settings = handle?.relaySettings() {
            relayEnabled = settings.enabled
            relayCustomURL = settings.customUrl
            relayBuiltIn = settings.builtInUrl
        }
        systemComponents = systemComponentsInstalled()
        microphoneDriver = microphoneDriverInstalled()
        openAtLogin = SMAppService.mainApp.status == .enabled
    }

    private func tick() {
        let allowed = Portputer.inputAllowed(ask: false)
        if allowed != inputAllowed {
            diagnostic("Typing permission: \(allowed ? "allowed" : "not allowed") (\(inputPermissionDetail()))")
        }
        inputAllowed = allowed
        if pairingCode != nil {
            pairingSecondsLeft = handle?.pairingSecondsLeft() ?? 0
            if pairingSecondsLeft == 0 { pairingCode = nil }
        }
        let now = handle?.connected() ?? []
        if now.map(\.id) != connected.map(\.id) { connected = now }
    }

    func newPairingCode() {
        guard let handle else { return }
        pairingCode = handle.newPairingCode()
        pairingSecondsLeft = handle.pairingSecondsLeft()
    }

    func cancelPairing() {
        handle?.cancelPairing()
        pairingCode = nil
    }

    func unpair(_ device: PairedDevice) {
        try? handle?.unpair(id: device.id)
        refresh()
        syncLoginWindow()
        announce("\(device.name) removed.")
    }

    func setOpenAtLogin(_ on: Bool) {
        do {
            if on {
                try SMAppService.mainApp.register()
            } else {
                try SMAppService.mainApp.unregister()
            }
        } catch {
            status = "Open at Login: \(error.localizedDescription)"
        }
        openAtLogin = SMAppService.mainApp.status == .enabled
    }

    /// Saves the internet relay settings and listens there, or stops.
    func saveRelay() {
        do {
            try handle?.setRelay(enabled: relayEnabled, customUrl: relayCustomURL.trimmingCharacters(in: .whitespaces))
            syncLoginWindow()
        } catch {
            relayStatus = error.localizedDescription
        }
    }

    /// Gives the server at the login window the current pairings.
    func syncLoginWindow() {
        guard systemComponents else { return }
        do {
            try Portputer.syncLoginWindow(storeDir: storeDir)
        } catch {
            status = "Login window: \(error.localizedDescription)"
        }
    }

    /// Installs or removes the Portputer Microphone driver, with an
    /// administrator's password. Core Audio restarts, so sound stops briefly.
    func setMicrophoneDriver(_ install: Bool) {
        let agent = Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/portputer-agent").path
        let escaped = agent.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"")
        let command = install ? "install-driver" : "uninstall-driver"
        let source = "do shell script quoted form of \"\(escaped)\" & \" \(command)\" with administrator privileges"
        var error: NSDictionary?
        NSAppleScript(source: source)?.executeAndReturnError(&error)
        if let error, (error[NSAppleScript.errorNumber] as? Int) != -128 {
            status = "Microphone driver: \(error[NSAppleScript.errorMessage] as? String ?? "unknown error")"
            return
        }
        // Core Audio takes a moment to come back with the driver loaded.
        DispatchQueue.main.asyncAfter(deadline: .now() + 2) {
            self.refresh()
            self.status = self.microphoneDriver ? "Portputer Microphone installed." : "Portputer Microphone removed."
            announce(self.status)
        }
    }

    /// Installs or removes the helper daemon and the login window server,
    /// with an administrator's password.
    func setSystemComponents(_ install: Bool) {
        let agent = Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/portputer-agent").path
        let path = install ? agent : "/Library/Application Support/Portputer/portputer-agent"
        let escaped = path.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"")
        let command = install ? "install" : "uninstall"
        let source = "do shell script quoted form of \"\(escaped)\" & \" \(command)\" with administrator privileges"
        var error: NSDictionary?
        NSAppleScript(source: source)?.executeAndReturnError(&error)
        if let error, (error[NSAppleScript.errorNumber] as? Int) != -128 {
            status = "\(install ? "Install" : "Removal") failed: \(error[NSAppleScript.errorMessage] as? String ?? "unknown error")"
        }
        // The daemon takes a moment to make its socket.
        DispatchQueue.main.asyncAfter(deadline: .now() + 1) {
            self.refresh()
            if install && self.systemComponents {
                self.syncLoginWindow()
                self.status = "Login window support installed."
                announce(self.status)
            } else if !install && !self.systemComponents {
                self.status = "Login window support removed."
                announce(self.status)
            }
        }
    }

    fileprivate func handle(_ event: ServerNotice) {
        switch event {
        case .listening(let port):
            self.port = port
        case .connected(_, let name, _):
            refresh()
            if announceConnections { announce("\(name) connected.") }
        case .disconnected(_, let name):
            refresh()
            if announceConnections { announce("\(name) disconnected.") }
        case .paired(_, let name):
            pairingCode = nil
            refresh()
            syncLoginWindow()
            announce("Paired with \(name).")
        case .pairingFailed(let name):
            status = "Pairing with \(name) failed: wrong code."
            announce(status)
        case .inputNotAllowed:
            inputAllowed = false
        case .relay(let listening, let message):
            relayStatus = listening ? "Reachable through the internet relay." : message
        case .audioFailed(let message):
            status = "Audio: \(message)"
        case .error(let message):
            status = message
        }
    }

    private final class Events: ServerListener {
        func event(event: ServerNotice) {
            Task { @MainActor in ServerModel.shared.handle(event) }
        }
    }
}

/// Appends a line to ~/Library/Application Support/Portputer/diagnostics.log.
@MainActor func diagnostic(_ line: String) {
    let url = ServerModel.supportDir.appendingPathComponent("diagnostics.log")
    let text = "\(Date()) \(line)\n"
    if let handle = try? FileHandle(forWritingTo: url) {
        handle.seekToEndOfFile()
        handle.write(Data(text.utf8))
        try? handle.close()
    } else {
        try? FileManager.default.createDirectory(at: ServerModel.supportDir, withIntermediateDirectories: true)
        try? Data(text.utf8).write(to: url)
    }
}

/// Has VoiceOver (or another screen reader) speak a message.
func announce(_ text: String) {
    NSAccessibility.post(
        element: NSApp as Any,
        notification: .announcementRequested,
        userInfo: [.announcement: text, .priority: NSAccessibilityPriorityLevel.high.rawValue]
    )
}

func openPrivacySettings(_ pane: String) {
    if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?\(pane)") {
        NSWorkspace.shared.open(url)
    }
}
