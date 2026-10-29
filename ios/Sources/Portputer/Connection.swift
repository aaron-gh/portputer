import AVFoundation
import Foundation

/// A computer in the list: paired, found on the network, or both.
struct Computer: Identifiable, Equatable {
    let id: String
    var known: KnownServer?
    var ads: [Advertisement]
    var reach: Reachability?

    var name: String { ads.first?.name ?? known?.name ?? id }
    var os: String { ads.first?.os ?? known?.os ?? "" }
    var userAd: Advertisement? { ads.first { $0.session == .user } }
    var loginAd: Advertisement? { ads.first { $0.session == .loginWindow } }

    /// The user session's server first, then the login window's, then where
    /// it was last, then the relay, which the core tries after it.
    var target: Target? {
        if let ad = userAd ?? loginAd, let host = ad.addresses.first {
            return Target(host: host, port: ad.port, id: id, name: name)
        }
        if let k = known, !k.host.isEmpty || k.hasRelay {
            return Target(host: k.host, port: k.port, id: id, name: name)
        }
        return nil
    }
}

struct Target: Equatable {
    let host: String
    let port: UInt16
    let id: String?
    let name: String
}

/// The one connection to a computer, and the computers to choose from.
@MainActor
final class Connection: ObservableObject {
    static let shared = Connection()

    enum State: Equatable {
        case idle
        case connecting(Target)
        case connected(ServerInfo)
        case reconnecting(ServerInfo)
        case closed(name: String, reason: String)
    }

    @Published private(set) var state: State = .idle
    @Published private(set) var computers: [Computer] = []
    @Published private(set) var audioOn = false
    @Published private(set) var audioWanted = false
    @Published private(set) var micWanted = false
    @Published private(set) var micOn = false
    /// Use Internet Relay: off for each connection.
    @Published private(set) var useRelay = false
    @Published private(set) var relayAvailable = false
    /// Modifiers held from Keys and Shortcuts.
    @Published private(set) var held: Set<String> = []
    @Published var message = ""
    @Published var sendKeyboard = Settings.sendKeyboard {
        didSet {
            Settings.sendKeyboard = sendKeyboard
            if !sendKeyboard { session?.releaseAll() }
        }
    }
    @Published var shareClipboard = Settings.shareClipboard {
        didSet {
            Settings.shareClipboard = shareClipboard
            updateClipboard()
        }
    }
    @Published var muteSpeakers = Settings.muteSpeakers {
        didSet {
            Settings.muteSpeakers = muteSpeakers
            session?.setSpeakersMuted(muted: muteSpeakers)
        }
    }
    @Published var volume = Settings.volume {
        didSet {
            Settings.volume = volume
            session?.setVolume(volume: Float(volume / 100))
        }
    }

    private var session: ClientSession?
    private var clipboard: ClipboardSync?
    private var bonjour: Bonjour?
    private var ads: [String: Advertisement] = [:]
    private var reach: [String: Reachability] = [:]
    private var probeTimer: Timer?
    private var probing = false
    private var generation = 0

    var info: ServerInfo? {
        switch state {
        case .connected(let i), .reconnecting(let i): return i
        default: return nil
        }
    }

    /// Connected to a paired computer: keys and commands go through.
    var canSend: Bool {
        if case .connected(let i) = state { return i.trusted }
        return false
    }

    /// The session, for transfers, which run off the main thread.
    var transferSession: ClientSession? { canSend ? session : nil }

    // MARK: Computers

    /// Runs from the first time the computer list shows: the core also uses
    /// what it finds to reconnect at new addresses.
    func startDiscovery() {
        guard bonjour == nil else { return }
        ads = [:]
        bonjour = Bonjour(
            found: { ad in Task { @MainActor in Connection.shared.found(ad) } },
            lost: { name in Task { @MainActor in Connection.shared.lost(name) } }
        )
        bonjour?.start()
        refresh()
        probe()
        probeTimer = Timer.scheduledTimer(withTimeInterval: 15, repeats: true) { _ in
            Task { @MainActor in Connection.shared.probe() }
        }
    }

    private func found(_ ad: Advertisement) {
        ads[ad.fullname] = ad
        refresh()
    }

    private func lost(_ name: String) {
        ads[name] = nil
        refresh()
    }

    func refresh() {
        let known = Dictionary(uniqueKeysWithValues: knownServers(storeDir: Settings.storeDir).map { ($0.id, $0) })
        let byId = Dictionary(grouping: ads.values, by: \.id)
        computers = Set(known.keys).union(byId.keys).map { id in
            Computer(id: id, known: known[id], ads: byId[id] ?? [], reach: reach[id])
        }
        .sorted { ($0.known == nil ? 1 : 0, $0.name.lowercased()) < ($1.known == nil ? 1 : 0, $1.name.lowercased()) }
    }

    /// Checks the paired computers that discovery doesn't see on this network.
    func probe() {
        guard !probing else { return }
        let ids = computers.filter { $0.known != nil && $0.ads.isEmpty && $0.id != info?.id }.map(\.id)
        guard !ids.isEmpty else { return }
        probing = true
        let store = Settings.storeDir
        DispatchQueue.global().async {
            let results = Dictionary(uniqueKeysWithValues: ids.map { ($0, probeComputer(storeDir: store, id: $0)) })
            Task { @MainActor in
                let c = Connection.shared
                c.probing = false
                c.reach.merge(results) { _, new in new }
                c.refresh()
            }
        }
    }

    func detail(_ computer: Computer) -> String {
        var parts = [computer.os == "mac" ? "Mac" : computer.os == "windows" ? "Windows" : "Computer"]
        if info?.id == computer.id {
            parts.append("connected")
        } else if let ad = computer.userAd {
            parts.append(ad.user.isEmpty ? "available" : "\(ad.user) logged in")
        } else if computer.loginAd != nil {
            parts.append("at the login window")
        } else if computer.known != nil {
            switch computer.reach {
            case .none: parts.append("checking")
            case .direct(_, let tailscale)?: parts.append(tailscale ? "available through Tailscale" : "available on another network")
            case .relay?: parts.append("available through the internet relay")
            case .notReachable?: parts.append("not reachable")
            }
        } else {
            parts.append("not reachable")
        }
        if computer.known == nil { parts.append("not paired") }
        return parts.joined(separator: ", ")
    }

    func forget(_ computer: Computer) {
        if info?.id == computer.id { disconnect() }
        try? forgetServer(storeDir: Settings.storeDir, id: computer.id)
        message = "\(computer.name) forgotten."
        refresh()
    }

    // MARK: Connection

    func connect(_ target: Target) {
        disconnect()
        generation += 1
        let gen = generation
        state = .connecting(target)
        let store = Settings.storeDir
        let device = Settings.deviceName
        DispatchQueue.global().async {
            do {
                let s = try ClientSession.connect(host: target.host, port: target.port, expectedId: target.id, storeDir: store, deviceName: device, localClipboard: false, listener: Events(gen: gen))
                Task { @MainActor in
                    let c = Connection.shared
                    guard gen == c.generation else {
                        s.disconnect()
                        return
                    }
                    c.session = s
                    s.setLatencyMs(ms: UInt32(Settings.bufferMs))
                    s.setVolume(volume: Float(c.volume / 100))
                    s.setSpeakersMuted(muted: c.muteSpeakers)
                    if let info = s.info() { c.connected(info) }
                }
            } catch {
                Task { @MainActor in
                    let c = Connection.shared
                    if gen == c.generation {
                        c.state = .closed(name: target.name, reason: "Couldn't connect to \(target.name). \(error.localizedDescription)")
                    }
                }
            }
        }
    }

    private func connected(_ info: ServerInfo) {
        let wasTrusted = self.info?.trusted ?? false
        state = .connected(info)
        relayAvailable = session?.relayAvailable() ?? false
        updateClipboard()
        if info.trusted && !wasTrusted {
            refresh()
            if Settings.audioOnConnect && info.audio && !audioWanted { setAudio(true) }
        }
    }

    func disconnect() {
        generation += 1
        clipboard?.stop()
        clipboard = nil
        session?.disconnect()
        session = nil
        resetMedia()
        if state != .idle { state = .idle }
    }

    private func resetMedia() {
        audioOn = false
        audioWanted = false
        micWanted = false
        micOn = false
        useRelay = false
        relayAvailable = false
        held = []
        AudioSession.update(active: false)
    }

    func pair(_ code: String, done: @escaping @MainActor (String?) -> Void) {
        guard let s = session else { return done("Not connected.") }
        DispatchQueue.global().async {
            var failure: String?
            do {
                try s.pair(code: code)
            } catch {
                failure = error.localizedDescription
            }
            let result = failure
            Task { @MainActor in done(result) }
        }
    }

    private func updateClipboard() {
        let want = info.map { $0.trusted && $0.files } == true && canSend && shareClipboard
        if want, clipboard == nil, let s = session, let name = info?.name {
            clipboard = ClipboardSync(session: s, computer: name)
            clipboard?.start()
        } else if !want, clipboard != nil {
            clipboard?.stop()
            clipboard = nil
        }
    }

    /// As Portputer comes to the front: sends the clipboard if it changed.
    func becameActive() {
        clipboard?.sendIfChanged()
    }

    /// Portputer put files on the clipboard, so they aren't sent back.
    func clipboardWritten() {
        clipboard?.noteWritten()
    }

    // MARK: Keys and commands

    func key(_ code: String, down: Bool, repeat isRepeat: Bool) {
        guard canSend else { return }
        session?.key(code: code, down: down, repeat: isRepeat)
    }

    func tap(_ code: String, modifiers: [String]) {
        guard canSend else { return }
        session?.tap(code: code, modifiers: modifiers)
    }

    func text(_ text: String) {
        guard canSend else { return }
        session?.text(text: text)
    }

    func hold(_ code: String, down: Bool) {
        guard canSend else { return }
        session?.key(code: code, down: down, repeat: false)
        if down { held.insert(code) } else { held.remove(code) }
    }

    func releaseAll() {
        session?.releaseAll()
        held = []
    }

    func power(_ action: Power, user: String? = nil, password: String? = nil) {
        guard canSend else { return }
        session?.power(action: action, user: user, password: password)
    }

    // MARK: Audio

    func setAudio(_ on: Bool) {
        audioWanted = on
        if on {
            AudioSession.update(active: true)
            session?.startAudio()
        } else {
            session?.stopAudio()
            AudioSession.update(active: micWanted)
        }
    }

    func setMic(_ on: Bool) {
        guard on else {
            micWanted = false
            session?.stopMic()
            AudioSession.update(active: audioWanted)
            return
        }
        micWanted = true
        AVAudioApplication.requestRecordPermission { granted in
            Task { @MainActor in
                let c = Connection.shared
                guard c.micWanted else { return }
                if granted {
                    AudioSession.update(active: true)
                    c.session?.startMic()
                } else {
                    c.micWanted = false
                    c.message = "Microphone access is off for Portputer, in iOS Settings, Privacy and Security, Microphone."
                }
            }
        }
    }

    /// After a phone call or another app's audio interruption: the streams
    /// stopped, so they're started again.
    func restartAudio() {
        guard canSend else { return }
        AudioSession.update(active: audioWanted || micWanted)
        if audioWanted {
            session?.stopAudio()
            session?.startAudio()
        }
        if micWanted {
            session?.stopMic()
            session?.startMic()
        }
    }

    func setBuffer(_ ms: Int) {
        session?.setLatencyMs(ms: UInt32(ms))
    }

    func setUseRelay(_ on: Bool) {
        useRelay = on
        session?.setUseRelay(on: on)
    }

    // MARK: Events from the core

    private final class Events: SessionListener {
        let gen: Int
        init(gen: Int) { self.gen = gen }

        private func onMain(_ block: @escaping @MainActor (Connection) -> Void) {
            let gen = gen
            Task { @MainActor in
                let c = Connection.shared
                if c.generation == gen { block(c) }
            }
        }

        func connected(info: ServerInfo) { onMain { $0.connected(info) } }

        func reconnecting() {
            onMain { c in
                guard let info = c.info else { return }
                c.state = .reconnecting(info)
                c.audioOn = false
                c.held = []
            }
        }

        func closed(reason: String) {
            onMain { c in
                let name: String
                if case .connecting(let t) = c.state { name = t.name } else { name = c.info?.name ?? "" }
                c.clipboard?.stop()
                c.clipboard = nil
                c.session = nil
                c.resetMedia()
                c.state = .closed(name: name, reason: "Disconnected from \(name). \(reason)")
            }
        }

        func notice(code: String, message: String) { onMain { $0.message = message } }

        func error(code: String, message: String) {
            onMain { c in
                if code == "audio_failed" { c.audioWanted = false }
                if code == "mic_failed" { c.micWanted = false }
                if code == "relay_failed" { c.useRelay = false }
                c.message = message
            }
        }

        func audioStarted(sampleRate: UInt32) { onMain { $0.audioOn = true } }
        func audioStopped(reason: String) { onMain { $0.audioOn = false } }
        func micStarted() { onMain { $0.micOn = true } }
        func micStopped(reason: String) { onMain { $0.micOn = false } }
        func clipboardText(text: String) { onMain { $0.clipboard?.received(text: text) } }
        func clipboardFiles(files: [FileInfo]) { onMain { $0.clipboard?.received(files: files) } }
        func clipboardSynced(message: String) { onMain { $0.message = message } }
    }
}

/// The app's audio session: playback and recording together, so Send
/// Microphone can start while the computer's audio plays, and so audio
/// continues in the background.
enum AudioSession {
    static func configure() {
        let s = AVAudioSession.sharedInstance()
        try? s.setCategory(.playAndRecord, mode: .default, options: [.defaultToSpeaker, .allowBluetoothA2DP])
    }

    static func update(active: Bool) {
        try? AVAudioSession.sharedInstance().setActive(active, options: active ? [] : [.notifyOthersOnDeactivation])
    }
}
