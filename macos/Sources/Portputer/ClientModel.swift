import AppKit
import Foundation

/// This Mac as the controlling side: finding computers, the connection, and
/// sending the keyboard and commands.
@MainActor
final class ClientModel: ObservableObject {
    static let shared = ClientModel()

    enum State: Equatable {
        case idle
        case connecting(String)
        case connected(ServerInfo)
        case reconnecting(ServerInfo)
        case closed(String)
    }

    struct Computer: Identifiable, Equatable {
        let id: String
        var name: String
        var os: String
        var known: KnownServer?
        var ads: [Advertisement]

        var userAd: Advertisement? { ads.first { $0.session == .user } }
        var loginAd: Advertisement? { ads.first { $0.session == .loginWindow } }
        var macs: [String] { Array(Set(ads.flatMap(\.macs) + (known?.macs ?? []))) }

        var detail: String {
            var parts = [os == "mac" ? "Mac" : os == "windows" ? "Windows" : "Computer"]
            if let ad = userAd {
                parts.append(ad.user.isEmpty ? "available" : "\(ad.user) logged in")
            } else if loginAd != nil {
                parts.append("at the login window")
            } else {
                parts.append("not on the network")
            }
            if known == nil { parts.append("not paired") }
            return parts.joined(separator: ", ")
        }
    }

    @Published private(set) var state: State = .idle
    @Published private(set) var computers: [Computer] = []
    @Published private(set) var capturing = false
    @Published private(set) var audioOn = false
    @Published private(set) var audioWanted = false
    @Published private(set) var micWanted = false
    @Published private(set) var micOn = false
    @Published var message = ""
    @Published var volume: Double = UserDefaults.standard.object(forKey: "clientVolume") as? Double ?? 1 {
        didSet {
            UserDefaults.standard.set(volume, forKey: "clientVolume")
            session?.setVolume(volume: Float(volume))
        }
    }

    @Published var muteSpeakers: Bool = UserDefaults.standard.bool(forKey: "clientMuteSpeakers") {
        didSet {
            UserDefaults.standard.set(muteSpeakers, forKey: "clientMuteSpeakers")
            session?.setSpeakersMuted(muted: muteSpeakers)
        }
    }

    private var session: ClientSession?
    private var discovery: Discovery?
    private var ads: [String: Advertisement] = [:]
    private var generation = 0

    let storeDir = ServerModel.supportDir.appendingPathComponent("client", isDirectory: true).path

    var info: ServerInfo? {
        switch state {
        case .connected(let i), .reconnecting(let i): return i
        default: return nil
        }
    }

    var canSend: Bool {
        if case .connected(let i) = state { return i.trusted }
        return false
    }

    func startDiscovery() {
        try? FileManager.default.createDirectory(atPath: storeDir, withIntermediateDirectories: true)
        guard discovery == nil else { return }
        ads = [:]
        discovery = try? Discovery.start(listener: Browse())
        refresh()
    }

    func stopDiscovery() {
        discovery?.stop()
        discovery = nil
    }

    fileprivate func found(_ ad: Advertisement) {
        ads[ad.fullname] = ad
        refresh()
    }

    fileprivate func lost(_ name: String) {
        ads[name] = nil
        refresh()
    }

    func refresh() {
        let known = Dictionary(uniqueKeysWithValues: knownServers(storeDir: storeDir).map { ($0.id, $0) })
        let byId = Dictionary(grouping: ads.values, by: \.id)
        // Not this Mac's own server.
        let ids = Set(known.keys).union(byId.keys).subtracting([ServerModel.shared.serverId])
        computers = ids.map { id in
            let a = byId[id] ?? []
            return Computer(id: id, name: a.first?.name ?? known[id]?.name ?? id, os: a.first?.os ?? known[id]?.os ?? "", known: known[id], ads: a)
        }
        .sorted { ($0.known == nil ? 1 : 0, $0.name.lowercased()) < ($1.known == nil ? 1 : 0, $1.name.lowercased()) }
    }

    func connect(_ computer: Computer) {
        let ad = computer.userAd ?? computer.loginAd
        if let ad, let host = ad.addresses.first {
            connect(host: host, port: ad.port, id: computer.id, name: computer.name)
        } else if let k = computer.known, !k.host.isEmpty {
            connect(host: k.host, port: k.port, id: computer.id, name: computer.name)
        } else {
            message = "\(computer.name) is not on the network."
        }
    }

    func connect(host: String, port: UInt16, id: String?, name: String) {
        disconnect()
        generation += 1
        let gen = generation
        state = .connecting(name)
        let store = storeDir
        let device = computerName()
        DispatchQueue.global().async {
            do {
                let s = try ClientSession.connect(host: host, port: port, expectedId: id, storeDir: store, deviceName: device, localClipboard: true, listener: Events(gen: gen))
                Task { @MainActor in
                    let model = ClientModel.shared
                    guard gen == model.generation else {
                        s.disconnect()
                        return
                    }
                    model.session = s
                    s.setVolume(volume: Float(model.volume))
                    s.setSpeakersMuted(muted: model.muteSpeakers)
                    s.setLatencyMs(ms: 40)
                    if let info = s.info() { model.connected(info) }
                }
            } catch {
                Task { @MainActor in
                    let model = ClientModel.shared
                    if gen == model.generation { model.state = .closed("Couldn't connect to \(name). \(error.localizedDescription)") }
                }
            }
        }
    }

    fileprivate func connected(_ info: ServerInfo) {
        let wasTrusted = self.info?.trusted ?? false
        state = .connected(info)
        if info.trusted && !wasTrusted {
            refresh()
            if info.audio { setAudio(true) }
        }
    }

    func disconnect() {
        generation += 1
        setCapturing(false)
        session?.disconnect()
        session = nil
        audioOn = false
        audioWanted = false
        micWanted = false
        micOn = false
        state = .idle
    }

    func pair(_ code: String) {
        guard let s = session else { return }
        message = "Pairing."
        DispatchQueue.global().async {
            let result: String?
            do {
                try s.pair(code: code)
                result = nil
            } catch {
                result = error.localizedDescription
            }
            Task { @MainActor in
                let model = ClientModel.shared
                model.message = result.map { "Pairing failed. \($0)" } ?? "Paired with \(model.info?.name ?? "")."
                announce(model.message)
            }
        }
    }

    func setCapturing(_ on: Bool) {
        guard on != capturing else { return }
        if !on { session?.releaseAll() }
        capturing = on
    }

    func setAudio(_ on: Bool) {
        audioWanted = on
        on ? session?.startAudio() : session?.stopAudio()
    }

    func setMic(_ on: Bool) {
        micWanted = on
        on ? session?.startMic() : session?.stopMic()
    }

    func key(_ code: String, down: Bool, repeat isRepeat: Bool = false) {
        guard canSend else { return }
        session?.key(code: code, down: down, repeat: isRepeat)
    }

    /// Uploads files to the computer's Downloads folder.
    func sendFiles(_ urls: [URL]) {
        guard canSend, let s = session else { return }
        let name = info?.name ?? ""
        message = "Sending \(urls.count == 1 ? "1 file" : "\(urls.count) files")."
        announce(message)
        DispatchQueue.global().async {
            var sent = 0
            var failure: String?
            for url in urls {
                do {
                    _ = try s.upload(path: url.path, name: nil, clipboard: nil, listener: NoProgress())
                    sent += 1
                } catch {
                    failure = error.localizedDescription
                }
            }
            let text = failure.map { "Sending failed: \($0)" } ?? "Sent \(sent == 1 ? "1 file" : "\(sent) files") to Downloads on \(name)."
            Task { @MainActor in
                ClientModel.shared.message = text
                announce(text)
            }
        }
    }

    private final class NoProgress: ProgressListener {
        func progress(done: UInt64, total: UInt64) {}
    }

    func releaseAll() {
        session?.releaseAll()
        message = "All keys released."
    }

    func tap(_ code: String, modifiers: [String]) {
        guard canSend else { return }
        session?.tap(code: code, modifiers: modifiers)
    }

    func text(_ text: String) {
        guard canSend else { return }
        session?.text(text: text)
    }

    func power(_ action: Power, user: String? = nil, password: String? = nil) {
        guard canSend else { return }
        session?.power(action: action, user: user, password: password)
    }

    func wake(_ computer: Computer) {
        message = Portputer.wake(macs: computer.macs) ? "Wake on LAN packet sent to \(computer.name)." : "Wake on LAN packet not sent."
        announce(message)
    }

    func forget(_ computer: Computer) {
        if info?.id == computer.id { disconnect() }
        try? forgetServer(storeDir: storeDir, id: computer.id)
        refresh()
    }

    private final class Browse: DiscoveryListener {
        func found(ad: Advertisement) { Task { @MainActor in ClientModel.shared.found(ad) } }
        func lost(fullname: String) { Task { @MainActor in ClientModel.shared.lost(fullname) } }
    }

    private final class Events: SessionListener {
        let gen: Int
        init(gen: Int) { self.gen = gen }

        private func onMain(_ block: @escaping @MainActor (ClientModel) -> Void) {
            let gen = gen
            Task { @MainActor in
                let model = ClientModel.shared
                if model.generation == gen { block(model) }
            }
        }

        func connected(info: ServerInfo) { onMain { $0.connected(info) } }

        func reconnecting() {
            onMain { m in
                if let info = m.info {
                    m.state = .reconnecting(info)
                    m.audioOn = false
                    announce("Connection to \(info.name) lost. Reconnecting.")
                }
            }
        }

        func closed(reason: String) {
            onMain { m in
                let name = m.info?.name ?? ""
                m.setCapturing(false)
                m.session = nil
                m.audioOn = false
                m.audioWanted = false
                m.state = .closed("Disconnected from \(name). \(reason)")
                announce("Disconnected from \(name).")
            }
        }

        func notice(code: String, message: String) { onMain { $0.message = message } }

        func error(code: String, message: String) {
            onMain { m in
                if code == "audio_failed" { m.audioWanted = false }
                if code == "mic_failed" { m.micWanted = false }
                m.message = message
                announce(message)
            }
        }

        func audioStarted(sampleRate: UInt32) { onMain { $0.audioOn = true } }
        func micStarted() { onMain { $0.micOn = true } }
        // This Mac's clipboard is kept in step by the core, so these two don't come.
        func clipboardText(text: String) {}
        func clipboardFiles(files: [FileInfo]) {}
        func clipboardSynced(message: String) {
            onMain { m in
                m.message = message
                announce(message)
            }
        }
        func micStopped(reason: String) { onMain { $0.micOn = false } }
        func audioStopped(reason: String) { onMain { $0.audioOn = false } }
    }
}
