import AppKit
import SwiftUI

@main
struct PortputerApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @StateObject private var server = ServerModel.shared
    @StateObject private var client = ClientModel.shared
    @StateObject private var updater = Updater.shared

    var body: some Scene {
        MenuBarExtra("Portputer", systemImage: "keyboard.badge.ellipsis") {
            MenuContent()
                .environmentObject(server)
                .environmentObject(client)
                .environmentObject(updater)
        }

        Window("Portputer Settings", id: Windows.settings) {
            SettingsView()
                .environmentObject(server)
                .frame(minWidth: 480, minHeight: 420)
        }
        .windowResizability(.contentSize)

        Window("Pair New Device", id: Windows.pair) {
            PairingView()
                .environmentObject(server)
        }
        .windowResizability(.contentSize)

        Window("Connect to Computer", id: Windows.client) {
            ClientView()
                .environmentObject(client)
                .frame(minWidth: 520, minHeight: 480)
        }
    }
}

/// The version people see, from the app's Info.plist (the workspace's, in Cargo.toml).
let appVersion = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "unknown"

enum Windows {
    static let settings = "settings"
    static let pair = "pair"
    static let client = "client"

    /// Set by the menu, so the app delegate can open windows too.
    @MainActor static var open: ((String) -> Void)?

    @MainActor static func show(_ id: String) {
        NSApp.activate(ignoringOtherApps: true)
        open?(id)
    }
}

struct MenuContent: View {
    @EnvironmentObject var server: ServerModel
    @EnvironmentObject var client: ClientModel
    @EnvironmentObject var updater: Updater
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Group {
            Text(server.running ? "Server on, port \(server.port)" : "Server off")
            if !server.connected.isEmpty {
                Text("Connected: \(server.connected.map(\.name).joined(separator: ", "))")
            }
            if server.systemComponentsOutdated || server.microphoneDriverOutdated {
                Button("Update Login Window Support and Driver in Settings…") { Windows.show(Windows.settings) }
            }
            if !server.inputAllowed {
                Button("Allow Typing (Accessibility Permission)…") {
                    openPrivacySettings("Privacy_Accessibility")
                }
            }
            Divider()
            Button("Pair New Device…") {
                server.newPairingCode()
                Windows.show(Windows.pair)
            }
            .disabled(!server.running)
            Button("Connect to Computer…") { Windows.show(Windows.client) }
            Button("Settings…") { Windows.show(Windows.settings) }
                .keyboardShortcut(",")
            Divider()
            Text("Portputer \(appVersion)")
            CheckForUpdatesMenuItem(updater: updater)
            Button("Quit Portputer") { NSApp.terminate(nil) }
                .keyboardShortcut("q")
        }
        .onAppear { Windows.open = { openWindow(id: $0) } }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        Task { @MainActor in
            ServerModel.shared.launch()
            // The windows can only open once SwiftUI has made the menu;
            // until then, open them by hand on first launch.
            if !UserDefaults.standard.bool(forKey: "launchedBefore") {
                UserDefaults.standard.set(true, forKey: "launchedBefore")
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { Windows.show(Windows.settings) }
            }
        }
    }

    /// Opening the app again (from Finder or Spotlight) shows Settings, as
    /// it has no Dock icon.
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        Task { @MainActor in Windows.show(Windows.settings) }
        return false
    }

    func applicationWillTerminate(_ notification: Notification) {
        Task { @MainActor in ClientModel.shared.disconnect() }
    }
}
