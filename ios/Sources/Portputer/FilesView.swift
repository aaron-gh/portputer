import SwiftUI

/// The computer's files: folders to browse, files to download to Portputer's
/// folder in the Files app, and Send Files, which uploads files from this
/// device to the computer's Downloads folder.
struct FilesView: View {
    @EnvironmentObject var connection: Connection
    @State private var listing: FileListing?
    @State private var status = ""
    @State private var busy = false
    @State private var picking = false

    var body: some View {
        List {
            Section {
                Button("Send Files") { picking = true }
                if let places = listing?.places, !places.isEmpty {
                    Menu("Places") {
                        ForEach(places, id: \.path) { place in
                            Button(place.name) { open(place.path) }
                        }
                    }
                }
            } footer: {
                MessageText(text: status)
            }
            if let listing {
                Section(listing.path) {
                    if let parent = listing.parent {
                        Button {
                            open(parent)
                        } label: {
                            row("Parent folder", folderName(parent))
                        }
                        .accessibilityElement(children: .ignore)
                        .accessibilityLabel("Parent folder")
                        .accessibilityAddTraits(.isButton)
                    }
                    ForEach(listing.entries, id: \.path) { entry in
                        Button {
                            entry.dir ? open(entry.path) : download(entry)
                        } label: {
                            row(entry.name, detail(entry))
                        }
                        .accessibilityElement(children: .ignore)
                        .accessibilityLabel("\(entry.name), \(detail(entry))")
                        .accessibilityAddTraits(.isButton)
                        .accessibilityHint(entry.dir ? "Opens the folder." : "Downloads the file.")
                    }
                }
            }
        }
        .navigationTitle(connection.info.map { "Files on \($0.name)" } ?? "Files")
        .navigationBarTitleDisplayMode(.inline)
        .disabled(!connection.canSend)
        .onAppear { if listing == nil { open("") } }
        .fileImporter(isPresented: $picking, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
            if case .success(let urls) = result { send(urls) }
        }
    }

    private func row(_ name: String, _ detail: String) -> some View {
        VStack(alignment: .leading) {
            Text(name)
            Text(detail).font(.caption).foregroundStyle(.secondary)
        }
    }

    private func folderName(_ path: String) -> String {
        let name = URL(fileURLWithPath: path).lastPathComponent
        return name.isEmpty ? path : name
    }

    private func detail(_ e: FileEntry) -> String {
        let kind = e.dir ? "Folder" : fileSize(e.size)
        guard e.modified > 0 else { return kind }
        let date = Date(timeIntervalSince1970: TimeInterval(e.modified))
        return "\(kind), \(date.formatted(date: .abbreviated, time: .shortened))"
    }

    private func open(_ path: String) {
        guard let session = connection.transferSession else { return }
        status = "Loading."
        Task.detached {
            do {
                let l = try session.listFiles(path: path)
                await MainActor.run {
                    listing = l
                    status = "\(folderName(l.path)), \(l.entries.count) items."
                }
            } catch {
                await MainActor.run { status = error.localizedDescription }
            }
        }
    }

    /// Runs a transfer off the main thread, one at a time.
    private func work(_ start: String, _ task: @escaping @Sendable (TransferProgress) throws -> String) {
        guard !busy else { return status = "A transfer is already running." }
        busy = true
        status = start
        let progress = TransferProgress { text in Task { @MainActor in status = text } }
        Task.detached {
            let result: String
            do {
                result = try task(progress)
            } catch {
                result = error.localizedDescription
            }
            await MainActor.run {
                busy = false
                status = result
            }
        }
    }

    private func download(_ entry: FileEntry) {
        guard let session = connection.transferSession else { return }
        work("Downloading \(entry.name).") { progress in
            let fm = FileManager.default
            let temp = fm.temporaryDirectory.appendingPathComponent("download/\(UUID().uuidString)", isDirectory: true)
            try fm.createDirectory(at: temp, withIntermediateDirectories: true)
            defer { try? fm.removeItem(at: temp) }
            progress.label = entry.name
            let saved = URL(fileURLWithPath: try session.download(path: entry.path, destDir: temp.path, listener: progress))
            let dest = Self.unusedName(saved.lastPathComponent, in: Settings.downloadsDir)
            try fm.moveItem(at: saved, to: dest)
            return "Saved \(dest.lastPathComponent) to Portputer in the Files app."
        }
    }

    private func send(_ urls: [URL]) {
        guard let session = connection.transferSession else { return }
        let name = connection.info?.name ?? ""
        work("Sending \(fileCount(urls.count)).") { progress in
            var sent = 0
            for url in urls {
                let scoped = url.startAccessingSecurityScopedResource()
                defer { if scoped { url.stopAccessingSecurityScopedResource() } }
                progress.label = url.lastPathComponent
                _ = try session.upload(path: url.path, name: url.lastPathComponent, clipboard: nil, listener: progress)
                sent += 1
            }
            return "Sent \(fileCount(sent)) to Downloads on \(name)."
        }
    }

    /// `name` in `dir`, numbered if a file has it already.
    private static func unusedName(_ name: String, in dir: URL) -> URL {
        let base = (name as NSString).deletingPathExtension
        let ext = (name as NSString).pathExtension
        var url = dir.appendingPathComponent(name)
        var n = 2
        while FileManager.default.fileExists(atPath: url.path) {
            url = dir.appendingPathComponent(ext.isEmpty ? "\(base) \(n)" : "\(base) \(n).\(ext)")
            n += 1
        }
        return url
    }

    /// Progress, said every 25 percent so it doesn't talk over everything.
    private final class TransferProgress: ProgressListener, @unchecked Sendable {
        var label = "" {
            didSet { last = -1 }
        }
        private var last = -1
        private let say: (String) -> Void

        init(say: @escaping (String) -> Void) {
            self.say = say
        }

        func progress(done: UInt64, total: UInt64) {
            guard total > 0 else { return }
            let step = Int(done * 100 / total) / 25 * 25
            if step > last && step < 100 {
                last = step
                say("\(label): \(step) percent.")
            }
        }
    }
}
