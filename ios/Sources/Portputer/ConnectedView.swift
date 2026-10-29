import SwiftUI

/// The connected computer: pairing, the keyboard and audio switches, and what
/// can be sent.
struct ConnectedView: View {
    @EnvironmentObject var connection: Connection
    @Binding var path: [Route]
    @State private var code = ""
    @State private var pairing = false
    @State private var typing = false
    @State private var text = ""

    var body: some View {
        Form {
            Section {
                Text(status)
                    .onChange(of: status) { _, new in announce(new) }
            } footer: {
                MessageText(text: connection.message)
            }
            switch connection.state {
            case .connected(let info) where !info.trusted:
                pairSection(info)
            case .connected(let info), .reconnecting(let info):
                controls(info)
                    .disabled(connection.state != .connected(info))
            default:
                EmptyView()
            }
            Section {
                Button(isClosed ? "Back to Computers" : "Disconnect") {
                    if !isClosed { connection.disconnect() }
                    path = []
                }
            }
        }
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.inline)
        .alert(connection.info.map { "Type Text on \($0.name)" } ?? "", isPresented: $typing) {
            TextField("Text", text: $text)
            Button("Type") {
                if !text.isEmpty {
                    connection.text(text)
                    connection.message = "Text typed."
                }
                text = ""
            }
            Button("Cancel", role: .cancel) { text = "" }
        }
    }

    private var isClosed: Bool {
        switch connection.state {
        case .closed, .idle: return true
        default: return false
        }
    }

    private var title: String {
        switch connection.state {
        case .connecting(let t): return t.name
        case .connected(let i), .reconnecting(let i): return i.name
        case .closed(let name, _): return name
        case .idle: return "Portputer"
        }
    }

    private var status: String {
        switch connection.state {
        case .idle: return "Not connected."
        case .connecting(let t): return "Connecting to \(t.name)."
        case .connected(let info): return info.sessionDescription
        case .reconnecting(let info): return "Connection to \(info.name) lost. Reconnecting."
        case .closed(_, let reason): return reason
        }
    }

    private func pairSection(_ info: ServerInfo) -> some View {
        Section("Pairing") {
            Text("On \(info.name), open Portputer and choose Pair New Device. Enter the code it shows.")
            TextField("Pairing code", text: $code)
                .textInputAutocapitalization(.characters)
                .autocorrectionDisabled()
                .onSubmit(pair)
            Button("Pair", action: pair)
                .disabled(code.trimmingCharacters(in: .whitespaces).isEmpty || pairing)
        }
    }

    private func pair() {
        let trimmed = code.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty, !pairing else { return }
        pairing = true
        connection.message = "Pairing."
        connection.pair(trimmed) { error in
            pairing = false
            if let error {
                connection.message = "Pairing failed. \(error)"
            } else {
                code = ""
                connection.message = "Paired with \(connection.info?.name ?? "")."
            }
        }
    }

    @ViewBuilder
    private func controls(_ info: ServerInfo) -> some View {
        Section {
            Toggle("Send Keyboard to Computer", isOn: $connection.sendKeyboard)
            Text(connection.sendKeyboard
                 ? "Keys from the hardware keyboard go to \(info.name) while Portputer is open. iOS keeps its own shortcuts, such as Command-Tab, and VoiceOver's keyboard commands while VoiceOver is on."
                 : "Keys from the hardware keyboard stay on this device.")
                .font(.caption)
        }
        Section {
            Toggle("Computer Audio", isOn: Binding(get: { connection.audioWanted }, set: { connection.setAudio($0) }))
                .disabled(!info.audio)
            Slider(value: $connection.volume, in: 0...100) { Text("Audio volume") }
                .accessibilityLabel("Audio volume")
                .disabled(!info.audio)
            if info.audio && info.mute {
                Toggle("Mute Computer Speakers", isOn: $connection.muteSpeakers)
            }
            if info.mic {
                Toggle("Send Microphone", isOn: Binding(get: { connection.micWanted }, set: { connection.setMic($0) }))
                Text("Apps on \(info.name) record this device's microphone from the Portputer Microphone input.")
                    .font(.caption)
            }
        }
        if info.files {
            Section {
                Toggle("Share Clipboard", isOn: $connection.shareClipboard)
                NavigationLink("Files", value: Route.files)
            }
        }
        if connection.relayAvailable {
            Section {
                Toggle("Use Internet Relay", isOn: Binding(get: { connection.useRelay }, set: { connection.setUseRelay($0) }))
                Text("Connects through the internet relay even when the computer is reachable directly, such as through Tailscale.")
                    .font(.caption)
            }
        }
        Section {
            NavigationLink("Keys and Shortcuts", value: Route.keys)
            Button("Type Text") { typing = true }
            if !info.power.isEmpty {
                NavigationLink("Power", value: Route.power)
            }
        }
    }
}

extension ServerInfo {
    var sessionDescription: String {
        let via = viaRelay ? " Through the internet relay." : ""
        if session == .loginWindow { return "Connected to \(name) at the login window.\(via)" }
        return user.isEmpty ? "Connected to \(name).\(via)" : "Connected to \(name), \(user) logged in.\(via)"
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
