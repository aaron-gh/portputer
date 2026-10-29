import UIKit
import UniformTypeIdentifiers

/// Keeps this device's clipboard and the computer's in step: text both ways,
/// and files, which are transferred and put on the other side's clipboard.
/// iOS lets only the app in front read the clipboard, so it's sent when it
/// changes while Portputer is open, and when Portputer comes to the front.
/// It's read only when its change count moves, as each read can show the
/// paste permission prompt.
@MainActor
final class ClipboardSync {
    /// Files copied on the computer up to this size are fetched by themselves.
    static let autoFetchLimit: UInt64 = 100 * 1024 * 1024

    private let session: ClientSession
    private let computer: String
    private var lastChange = UIPasteboard.general.changeCount
    private var observer: NSObjectProtocol?

    init(session: ClientSession, computer: String) {
        self.session = session
        self.computer = computer
    }

    func start() {
        session.setClipboardWatch(on: true)
        observer = NotificationCenter.default.addObserver(forName: UIPasteboard.changedNotification, object: nil, queue: .main) { _ in
            Task { @MainActor in Connection.shared.becameActive() }
        }
    }

    func stop() {
        observer.map(NotificationCenter.default.removeObserver)
        observer = nil
        session.setClipboardWatch(on: false)
    }

    func sendIfChanged() {
        let board = UIPasteboard.general
        guard board.changeCount != lastChange else { return }
        lastChange = board.changeCount
        if board.hasStrings, let text = board.string, !text.isEmpty {
            session.sendClipboardText(text: text)
            return
        }
        let providers = board.itemProviders
        guard !providers.isEmpty else { return }
        let session = session
        let computer = computer
        let batch = UInt64(Date().timeIntervalSince1970 * 1000)
        Task.detached {
            var sent = 0
            for provider in providers {
                guard let file = await Self.copy(provider) else { continue }
                defer { try? FileManager.default.removeItem(at: file.deletingLastPathComponent()) }
                do {
                    _ = try session.upload(path: file.path, name: file.lastPathComponent, clipboard: batch, listener: NoProgress())
                    sent += 1
                } catch {
                    await Self.say(error.localizedDescription)
                }
            }
            if sent > 0 { await Self.say("Copied \(fileCount(sent)) to \(computer).") }
        }
    }

    /// A clipboard item as a file in a temporary folder of its own.
    private nonisolated static func copy(_ provider: NSItemProvider) async -> URL? {
        guard let type = provider.registeredTypeIdentifiers.first else { return nil }
        let suggested = provider.suggestedName
        return await withCheckedContinuation { done in
            _ = provider.loadFileRepresentation(forTypeIdentifier: type) { url, _ in
                guard let url else { return done.resume(returning: nil) }
                var name = suggested ?? url.deletingPathExtension().lastPathComponent
                let ext = UTType(type)?.preferredFilenameExtension ?? url.pathExtension
                if URL(fileURLWithPath: name).pathExtension.isEmpty && !ext.isEmpty {
                    name += ".\(ext)"
                }
                let dir = FileManager.default.temporaryDirectory.appendingPathComponent("upload/\(UUID().uuidString)", isDirectory: true)
                let dest = dir.appendingPathComponent(name.replacingOccurrences(of: "/", with: "_"))
                do {
                    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
                    try FileManager.default.copyItem(at: url, to: dest)
                    done.resume(returning: dest)
                } catch {
                    done.resume(returning: nil)
                }
            }
        }
    }

    func received(text: String) {
        UIPasteboard.general.string = text
        lastChange = UIPasteboard.general.changeCount
    }

    func received(files: [FileInfo]) {
        let total = files.reduce(0) { $0 + $1.size }
        if total > Self.autoFetchLimit {
            Self.say("\(fileCount(files.count)) copied on \(computer), \(fileSize(total)): too large to copy here. Download them in Files.")
            return
        }
        let session = session
        let computer = computer
        Task.detached {
            let dir = FileManager.default.temporaryDirectory.appendingPathComponent("clipboard/\(UUID().uuidString)", isDirectory: true)
            do {
                try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
                let paths = try session.clipboardFiles(destDir: dir.path, listener: NoProgress())
                guard !paths.isEmpty else { return }
                await MainActor.run {
                    let providers = paths.compactMap { NSItemProvider(contentsOf: URL(fileURLWithPath: $0)) }
                    UIPasteboard.general.itemProviders = providers
                    Connection.shared.clipboardWritten()
                }
                await Self.say("Copied \(fileCount(paths.count)) from \(computer).")
            } catch {
                await Self.say(error.localizedDescription)
            }
        }
    }

    func noteWritten() {
        lastChange = UIPasteboard.general.changeCount
    }

    private static func say(_ text: String) {
        Connection.shared.message = text
    }
}

final class NoProgress: ProgressListener {
    func progress(done: UInt64, total: UInt64) {}
}
