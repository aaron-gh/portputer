import SwiftUI

/// Restart, shut down and the rest, for the actions the computer offers.
struct PowerView: View {
    @EnvironmentObject var connection: Connection
    @State private var confirming: Power?
    @State private var user = ""
    @State private var password = ""
    @State private var status = ""

    var body: some View {
        let info = connection.info
        let os = info?.os ?? "mac"
        List {
            Section {
                MessageText(text: status.isEmpty ? (info?.name ?? "") : status)
            }
            Section {
                ForEach(info?.power ?? [], id: \.self) { action in
                    VStack(alignment: .leading) {
                        Button(action.label(os: os)) { choose(action) }
                        if let help = help(action) {
                            Text(help).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }
            }
            if os == "mac", let info, !info.power.contains(.forceRestart) {
                Section {
                    Text("Force Restart and Restart Without FileVault Unlock need Portputer's system components, installed from Portputer on the Mac.")
                }
            }
        }
        .navigationTitle("Power")
        .navigationBarTitleDisplayMode(.inline)
        .disabled(!connection.canSend)
        .alert(confirming.map { "\($0.label(os: os)) \(info?.name ?? "")?" } ?? "", isPresented: Binding(get: { confirming != nil }, set: { if !$0 { confirming = nil } })) {
            if confirming == .unlockedRestart {
                TextField("User name", text: $user)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                SecureField("Password", text: $password)
            }
            Button(confirming?.label(os: os) ?? "OK", role: .destructive) {
                if let action = confirming {
                    if action == .unlockedRestart {
                        send(action, user: user.trimmingCharacters(in: .whitespaces), password: password)
                    } else {
                        send(action)
                    }
                }
                password = ""
                confirming = nil
            }
            Button("Cancel", role: .cancel) {
                password = ""
                confirming = nil
            }
        } message: {
            if let action = confirming, let help = help(action) { Text(help) }
        }
        .onAppear { user = info?.user ?? "" }
    }

    private func help(_ action: Power) -> String? {
        switch action {
        case .restart: "Apps are asked to quit. One with unsaved changes can stop the restart."
        case .forceRestart: "Restarts immediately. Unsaved changes are lost."
        case .unlockedRestart: "Restarts immediately, once, without the FileVault unlock screen, so the computer starts up to the login window and Portputer reconnects. Needs the password of a FileVault user."
        default: nil
        }
    }

    private func choose(_ action: Power) {
        if action == .lock {
            send(action)
        } else {
            confirming = action
        }
    }

    private func send(_ action: Power, user: String? = nil, password: String? = nil) {
        guard let info = connection.info else { return }
        connection.power(action, user: user, password: password)
        status = "\(action.label(os: info.os)) sent to \(info.name)."
    }
}
