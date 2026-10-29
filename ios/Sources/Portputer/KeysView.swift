import SwiftUI

/// Keys a keyboard may not have, and shortcuts, for the connected computer's
/// system. Activating a row presses the key; modifiers can also be held.
struct KeysView: View {
    @EnvironmentObject var connection: Connection
    @State private var status = ""

    var body: some View {
        let os = connection.info?.os ?? "mac"
        List {
            Section {
                Text(connection.info.map { "Keys go to \($0.name)." } ?? "")
                Button("Release All Keys") {
                    connection.releaseAll()
                    status = "All keys released."
                }
            } footer: {
                MessageText(text: status)
            }
            ForEach(keyMenu(os: os), id: \.title) { section in
                Section(section.title) {
                    ForEach(section.keys, id: \.label) { key in
                        row(key)
                    }
                }
            }
        }
        .navigationTitle("Keys and Shortcuts")
        .navigationBarTitleDisplayMode(.inline)
        .disabled(!connection.canSend)
    }

    private func row(_ key: MenuKey) -> some View {
        let held = connection.held.contains(key.code)
        return HStack {
            Button(key.label) {
                connection.tap(key.code, modifiers: key.modifiers)
                status = "\(key.label) pressed."
            }
            .accessibilityValue(held ? "held" : "")
            .accessibilityActions {
                if key.holdable {
                    Button(held ? "Release" : "Hold") { toggleHold(key) }
                }
            }
            if key.holdable {
                Spacer()
                Button(held ? "Release" : "Hold") { toggleHold(key) }
                    .buttonStyle(.bordered)
                    .accessibilityHidden(true)
            }
        }
    }

    private func toggleHold(_ key: MenuKey) {
        let holding = !connection.held.contains(key.code)
        connection.hold(key.code, down: holding)
        status = "\(key.label) \(holding ? "held" : "released")."
    }
}
