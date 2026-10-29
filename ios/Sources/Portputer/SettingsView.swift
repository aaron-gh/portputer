import SwiftUI

struct SettingsView: View {
    @EnvironmentObject var connection: Connection
    @State private var deviceName = Settings.deviceName
    @State private var byPosition = Settings.byPosition
    @State private var bufferMs = Settings.bufferMs
    @State private var audioOnConnect = Settings.audioOnConnect

    var body: some View {
        Form {
            Section {
                TextField("Device name shown on computers", text: $deviceName)
                    .onSubmit { Settings.deviceName = deviceName }
            } header: {
                Text("Device name shown on computers")
            }
            Section {
                Picker("Key mapping", selection: $byPosition) {
                    Text("By key position: the computer's keyboard layout applies").tag(true)
                    Text("By this device's keyboard layout").tag(false)
                }
                .pickerStyle(.inline)
                .onChange(of: byPosition) { _, on in Settings.byPosition = on }
            }
            Section {
                Picker("Audio buffer", selection: $bufferMs) {
                    ForEach(Settings.buffers, id: \.self) { ms in
                        Text(ms == 0 ? "Automatic" : "\(ms) ms").tag(ms)
                    }
                }
                .onChange(of: bufferMs) { _, ms in
                    Settings.bufferMs = ms
                    connection.setBuffer(ms)
                }
                Text("Automatic sets the buffer from the measured network jitter. A larger fixed buffer has fewer dropouts and more delay.")
                    .font(.caption)
                Toggle("Computer audio on connect", isOn: $audioOnConnect)
                    .onChange(of: audioOnConnect) { _, on in Settings.audioOnConnect = on }
            }
            Section {
                Text("Portputer version \(Settings.version)")
            }
        }
        .navigationTitle("Settings")
        .navigationBarTitleDisplayMode(.inline)
        .onDisappear { Settings.deviceName = deviceName }
    }
}
