import AVFoundation
import SwiftUI

@main
struct PortputerApp: App {
    @StateObject private var connection = Connection.shared
    @Environment(\.scenePhase) private var scenePhase

    init() {
        AudioSession.configure()
        NotificationCenter.default.addObserver(forName: AVAudioSession.interruptionNotification, object: nil, queue: .main) { note in
            let raw = note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt
            if raw.flatMap(AVAudioSession.InterruptionType.init) == .ended {
                Task { @MainActor in Connection.shared.restartAudio() }
            }
        }
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(connection)
        }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { connection.becameActive() }
        }
    }
}

enum Route: Hashable {
    case connected, keys, power, files, settings
}

struct RootView: View {
    @EnvironmentObject var connection: Connection
    @State private var path: [Route] = []

    var body: some View {
        NavigationStack(path: $path) {
            ComputersView(path: $path)
                .navigationDestination(for: Route.self) { route in
                    switch route {
                    case .connected: ConnectedView(path: $path)
                    case .keys: KeysView()
                    case .power: PowerView()
                    case .files: FilesView()
                    case .settings: SettingsView()
                    }
                }
        }
        .background(KeyCapture(active: connection.sendKeyboard && connection.canSend))
        .onChange(of: connection.state) { _, state in
            // Screens for the connected computer close when it's gone.
            if state == .idle { path.removeAll { $0 != .settings } }
            if connection.info == nil { path.removeAll { [.keys, .power, .files].contains($0) } }
        }
    }
}

/// The message line at the bottom of a screen, read out as it changes.
struct MessageText: View {
    let text: String

    var body: some View {
        Text(text)
            .accessibilityHidden(text.isEmpty)
            .onChange(of: text) { _, new in
                if !new.isEmpty { announce(new) }
            }
    }
}
