import SwiftUI

/// Connect to Computer: the computers on the network, and the connected one's
/// controls.
struct ClientView: View {
    @EnvironmentObject var model: ClientModel
    @State private var host = ""
    @State private var port = "47800"
    @State private var code = ""
    @State private var text = ""
    @State private var confirm: (Power, String)?
    @State private var credentials: (user: String, password: String) = ("", "")

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            switch model.state {
            case .idle, .closed:
                if case .closed(let reason) = model.state {
                    Text(reason)
                }
                computerList
            case .connecting(let name):
                Text("Connecting to \(name).")
                Button("Cancel") { model.disconnect() }
            case .reconnecting(let info):
                Text("Connection to \(info.name) lost. Reconnecting.")
                Button("Disconnect") { model.disconnect() }
            case .connected(let info):
                if info.trusted {
                    controls(info)
                } else {
                    pairing(info)
                }
            }
            if !model.message.isEmpty {
                Text(model.message)
                    .accessibilityAddTraits(.updatesFrequently)
            }
        }
        .padding(20)
        .onAppear { model.startDiscovery() }
        .onDisappear {
            if case .connected = model.state {} else { model.stopDiscovery() }
        }
        .onChange(of: model.capturing) { _, on in
            if on {
                KeyboardLock.shared.lock { model.setCapturing(false) }
                announce("Keyboard to \(model.info?.name ?? "computer"). Control-Command-Escape returns it.")
            } else {
                KeyboardLock.shared.unlock()
                announce("Keyboard to Mac.")
            }
        }
    }

    private var computerList: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Computers")
                .font(.headline)
                .accessibilityAddTraits(.isHeader)
            if model.computers.isEmpty {
                Text("No computers found. Portputer must be running on the computer, on the same network.")
            }
            List(model.computers) { computer in
                HStack {
                    VStack(alignment: .leading) {
                        Text(computer.name)
                        Text(computer.detail).font(.caption)
                    }
                    Spacer()
                    Button("Connect") { model.connect(computer) }
                    if computer.ads.isEmpty && !computer.macs.isEmpty {
                        Button("Wake") { model.wake(computer) }
                    }
                    if computer.known != nil {
                        Button("Forget") { model.forget(computer) }
                    }
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel("\(computer.name), \(computer.detail)")
                .accessibilityAddTraits(.isButton)
                .accessibilityAction { model.connect(computer) }
                .accessibilityActions {
                    if computer.ads.isEmpty && !computer.macs.isEmpty {
                        Button("Wake") { model.wake(computer) }
                    }
                    if computer.known != nil {
                        Button("Forget") { model.forget(computer) }
                    }
                }
            }
            .frame(minHeight: 180)
            Text("Connect by Address")
                .font(.headline)
                .accessibilityAddTraits(.isHeader)
            HStack {
                TextField("Host name or IP address", text: $host)
                TextField("Port", text: $port)
                    .frame(width: 80)
                Button("Connect") {
                    let h = host.trimmingCharacters(in: .whitespaces)
                    if !h.isEmpty { model.connect(host: h, port: UInt16(port) ?? 47800, id: nil, name: h) }
                }
            }
        }
    }

    private func pairing(_ info: ServerInfo) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Pairing")
                .font(.headline)
                .accessibilityAddTraits(.isHeader)
            Text("On \(info.name), open Portputer and choose Pair New Device. Enter the code it shows.")
            HStack {
                TextField("Pairing code", text: $code)
                    .onSubmit { model.pair(code) }
                Button("Pair") { model.pair(code) }
                    .disabled(code.isEmpty)
            }
            Button("Disconnect") { model.disconnect() }
        }
    }

    private func controls(_ info: ServerInfo) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(info.sessionDescription)
            Toggle("Send Keyboard to Computer", isOn: Binding(get: { model.capturing }, set: { model.setCapturing($0) }))
            Text(model.capturing
                 ? "Every key goes to \(info.name). Control-Command-Escape returns the keyboard to this Mac."
                 : "Control-Command-Escape returns the keyboard to this Mac once it's sent.")
                .font(.caption)
            KeyCapture(
                capturing: model.capturing,
                send: { code, down, isRepeat in
                    if let name = codeForMacKey(keyCode: code) { model.key(name, down: down, repeat: isRepeat) }
                },
                onEscape: { model.setCapturing(false) }
            )
            .frame(height: 1)
            Toggle("Computer Audio", isOn: Binding(get: { model.audioWanted }, set: { model.setAudio($0) }))
                .disabled(!info.audio)
            if info.mute {
                Toggle("Mute Computer Speakers", isOn: $model.muteSpeakers)
                    .disabled(!info.audio)
            }
            Slider(value: $model.volume, in: 0...1) { Text("Audio volume") }
                .disabled(!info.audio)
            if info.mic {
                Toggle("Send Microphone", isOn: Binding(get: { model.micWanted }, set: { model.setMic($0) }))
            }
            if model.relayAvailable {
                Toggle("Use Internet Relay", isOn: Binding(get: { model.useRelay }, set: { model.setUseRelay($0) }))
                Text("Connects through the internet relay even when \(info.name) is reachable directly, such as through Tailscale.")
                    .font(.caption)
            }
            HStack {
                Menu("Keys and Shortcuts") {
                    ForEach(keyMenu(os: info.os), id: \.title) { section in
                        Section(section.title) {
                            ForEach(section.keys, id: \.label) { key in
                                Button(key.label) { model.tap(key.code, modifiers: key.modifiers) }
                            }
                        }
                    }
                }
                Menu("Power") {
                    ForEach(info.power, id: \.self) { action in
                        Button(action.label(os: info.os)) { confirm = (action, action.label(os: info.os)) }
                    }
                }
                .disabled(info.power.isEmpty)
                Button("Release All Keys") { model.releaseAll() }
                if info.files {
                    Button("Send Files…") {
                        let panel = NSOpenPanel()
                        panel.allowsMultipleSelection = true
                        panel.canChooseDirectories = false
                        panel.message = "Choose files to send to Downloads on \(info.name)."
                        if panel.runModal() == .OK { model.sendFiles(panel.urls) }
                    }
                }
            }
            HStack {
                TextField("Text to type", text: $text)
                    .onSubmit(sendText)
                Button("Type", action: sendText)
                    .disabled(text.isEmpty)
            }
            Button("Disconnect") { model.disconnect() }
        }
        .alert(confirm.map { "\($0.1) \(info.name)?" } ?? "", isPresented: Binding(get: { confirm != nil }, set: { if !$0 { confirm = nil } })) {
            if confirm?.0 == .unlockedRestart {
                TextField("User name", text: $credentials.user)
                SecureField("Password", text: $credentials.password)
            }
            Button(confirm?.1 ?? "OK") {
                if let (action, _) = confirm {
                    if action == .unlockedRestart {
                        model.power(action, user: credentials.user, password: credentials.password)
                        credentials.password = ""
                    } else {
                        model.power(action)
                    }
                }
                confirm = nil
            }
            Button("Cancel", role: .cancel) { confirm = nil }
        }
        .onAppear { credentials.user = info.user }
    }

    private func sendText() {
        guard !text.isEmpty else { return }
        model.text(text)
        text = ""
        model.message = "Text typed."
    }
}

extension ServerInfo {
    var sessionDescription: String {
        let via = viaRelay ? " through the internet relay" : ""
        if session == .loginWindow { return "Connected to \(name) at the login window\(via)." }
        return user.isEmpty ? "Connected to \(name)\(via)." : "Connected to \(name), \(user) logged in\(via)."
    }
}

extension Power {
    func label(os: String) -> String {
        switch self {
        case .restart: "Restart"
        case .shutdown: "Shut Down"
        case .sleep: "Sleep"
        case .logOut: "Log Out"
        case .lock: "Lock Screen"
        case .forceRestart: "Force Restart"
        case .unlockedRestart: os == "mac" ? "Restart Without FileVault Unlock" : "Restart Without Disk Unlock"
        }
    }
}
