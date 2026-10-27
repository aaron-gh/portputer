import SwiftUI

struct SettingsView: View {
    @EnvironmentObject var server: ServerModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Form {
            Section("Server") {
                Toggle("Accept Connections", isOn: Binding(
                    get: { server.running },
                    set: { server.serverEnabled = $0 }
                ))
                Toggle("Open at Login", isOn: Binding(
                    get: { server.openAtLogin },
                    set: { server.setOpenAtLogin($0) }
                ))
                Toggle("Announce Connections", isOn: Binding(
                    get: { server.announceConnections },
                    set: { server.announceConnections = $0 }
                ))
                if !server.status.isEmpty {
                    Text(server.status)
                        .accessibilityAddTraits(.updatesFrequently)
                }
            }

            Section("Permissions") {
                LabeledContent("Accessibility (typing)") {
                    HStack {
                        Text(server.inputAllowed ? "Allowed" : "Not allowed")
                        if !server.inputAllowed {
                            Button("Open Privacy Settings") { openPrivacySettings("Privacy_Accessibility") }
                        }
                    }
                }
                LabeledContent("System Audio Recording") {
                    Button("Open Privacy Settings") { openPrivacySettings("Privacy_AudioCapture") }
                }
                Text("macOS asks for System Audio Recording the first time a device turns on Computer Audio.")
                    .font(.caption)
            }

            Section("Paired Devices") {
                if server.paired.isEmpty {
                    Text("None.")
                }
                ForEach(server.paired, id: \.id) { device in
                    HStack {
                        Text(device.name)
                        Spacer()
                        if server.connected.contains(where: { $0.id == device.id }) {
                            Text("Connected")
                        }
                        Button("Remove") { server.unpair(device) }
                            .accessibilityLabel("Remove \(device.name)")
                    }
                    .accessibilityElement(children: .combine)
                    .accessibilityAction(named: "Remove") { server.unpair(device) }
                }
                Button("Pair New Device…") {
                    server.newPairingCode()
                    openWindow(id: Windows.pair)
                }
                .disabled(!server.running)
            }

            Section("Internet Relay") {
                Toggle("Reachable Through the Internet Relay", isOn: Binding(
                    get: { server.relayEnabled },
                    set: {
                        server.relayEnabled = $0
                        server.saveRelay()
                    }
                ))
                Text(server.relayStatus)
                    .accessibilityAddTraits(.updatesFrequently)
                Text("Paired devices connect through the relay when this Mac isn't on their network. Only devices paired with this Mac can find or reach it there; the relay passes on encrypted data it can't read.")
                    .font(.caption)
                    .fixedSize(horizontal: false, vertical: true)
                DisclosureGroup("Custom Relay") {
                    TextField("Relay address", text: $server.relayCustomURL, prompt: Text(server.relayBuiltIn.isEmpty ? "https://relay.example.workers.dev" : server.relayBuiltIn))
                    Button("Use This Relay") { server.saveRelay() }
                }
            }

            Section("Microphone") {
                Text("Portputer Microphone is an audio input device for this Mac's apps. It records the microphone of a connected device that has Send Microphone on. Installs a driver with an administrator's password; installing or removing it stops all sound for a moment.")
                    .fixedSize(horizontal: false, vertical: true)
                LabeledContent("Portputer Microphone driver") {
                    HStack {
                        Text(server.microphoneDriver ? (server.microphoneDriverOutdated ? "Installed, from another version" : "Installed") : "Not installed")
                        if server.microphoneDriverOutdated {
                            Button("Update…") { server.setMicrophoneDriver(true) }
                        }
                        Button(server.microphoneDriver ? "Remove…" : "Install…") {
                            server.setMicrophoneDriver(!server.microphoneDriver)
                        }
                    }
                }
            }

            Section("Login Window") {
                Text("Lets paired devices type at the login window after a restart, before anyone logs in, and restart the Mac immediately or past the FileVault unlock screen. Installs a helper with an administrator's password.")
                    .fixedSize(horizontal: false, vertical: true)
                LabeledContent("Login window support") {
                    HStack {
                        Text(server.systemComponents ? (server.systemComponentsOutdated ? "Installed, from another version" : "Installed") : "Not installed")
                        if server.systemComponentsOutdated {
                            Button("Update…") { server.setSystemComponents(true) }
                        }
                        Button(server.systemComponents ? "Remove…" : "Install…") {
                            server.setSystemComponents(!server.systemComponents)
                        }
                    }
                }
                if server.systemComponents {
                    Button("Update Login Window Pairings") { server.syncLoginWindow() }
                }
            }
            Section("About") {
                LabeledContent("Version", value: appVersion)
                CheckForUpdatesMenuItem(updater: Updater.shared)
            }
        }
        .formStyle(.grouped)
        .onAppear { server.refresh() }
    }
}

struct PairingView: View {
    @EnvironmentObject var server: ServerModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            if let code = server.pairingCode {
                Text("Pairing code")
                    .font(.headline)
                    .accessibilityAddTraits(.isHeader)
                Text(code)
                    .font(.system(size: 34, weight: .semibold, design: .monospaced))
                    .textSelection(.enabled)
                    .accessibilityLabel(spelled(code))
                Text("In Portputer on the device, connect to \(computerName()) and enter this code. It works for \(minutes(server.pairingSecondsLeft)), or until five wrong tries.")
                    .fixedSize(horizontal: false, vertical: true)
                HStack {
                    Button("Copy Code") {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(code, forType: .string)
                    }
                    Button("New Code") { server.newPairingCode() }
                    Spacer()
                    Button("Cancel") {
                        server.cancelPairing()
                        dismiss()
                    }
                    .keyboardShortcut(.cancelAction)
                }
            } else {
                Text(server.paired.isEmpty ? "No pairing code." : "Paired devices: \(server.paired.map(\.name).joined(separator: ", ")).")
                HStack {
                    Button("New Code") { server.newPairingCode() }
                        .disabled(!server.running)
                    Spacer()
                    Button("Close") { dismiss() }
                        .keyboardShortcut(.cancelAction)
                }
            }
        }
        .padding(24)
        .frame(width: 440)
        .onAppear {
            if server.pairingCode == nil && server.running { server.newPairingCode() }
        }
    }

    /// The code read a character at a time, with the dashes as pauses.
    private func spelled(_ code: String) -> String {
        code.split(separator: "-").map { $0.map(String.init).joined(separator: " ") }.joined(separator: ", ")
    }

    private func minutes(_ seconds: UInt64) -> String {
        let m = max(1, (seconds + 59) / 60)
        return m == 1 ? "1 more minute" : "\(m) more minutes"
    }
}
