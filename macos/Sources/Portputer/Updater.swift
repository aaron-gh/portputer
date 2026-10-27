import Combine
import Sparkle
import SwiftUI

/// Updates through Sparkle: downloads, verifies Portputer's signature, and
/// offers Install and Relaunch in standard AppKit windows. Automatic checks
/// report only updates; Check for Updates always reports a result.
@MainActor
final class Updater: ObservableObject {
    static let shared = Updater()

    private let controller = SPUStandardUpdaterController(
        startingUpdater: true,
        updaterDelegate: nil,
        userDriverDelegate: nil
    )

    /// Whether a check can start now; false while one is already going.
    @Published var canCheckForUpdates = false

    init() {
        controller.updater.publisher(for: \.canCheckForUpdates)
            .assign(to: &$canCheckForUpdates)
    }

    func checkForUpdates() {
        NSApp.activate(ignoringOtherApps: true)
        controller.checkForUpdates(nil)
    }
}

/// The menu item that checks for updates.
struct CheckForUpdatesMenuItem: View {
    @ObservedObject var updater: Updater

    var body: some View {
        Button("Check for Updates…") { updater.checkForUpdates() }
            .disabled(!updater.canCheckForUpdates)
    }
}
