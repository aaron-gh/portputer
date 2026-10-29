import SwiftUI

/// Paired computers and computers on the network.
struct ComputersView: View {
    @EnvironmentObject var connection: Connection
    @Binding var path: [Route]
    @State private var addingAddress = false
    @State private var host = ""
    @State private var port = "47800"
    @State private var forgetting: Computer?

    var body: some View {
        List {
            Section("Computers") {
                if connection.computers.isEmpty {
                    Text("No computers found. Portputer must be running on the computer, on the same network. Add Computer by Address connects to one on another network.")
                }
                ForEach(connection.computers) { computer in
                    row(computer)
                }
            }
            Section {
                Button("Add Computer by Address") { addingAddress = true }
                NavigationLink("Settings", value: Route.settings)
            } footer: {
                MessageText(text: connection.message)
            }
        }
        .navigationTitle("Portputer")
        .onAppear { connection.startDiscovery() }
        .alert("Add Computer by Address", isPresented: $addingAddress) {
            TextField("Host name or IP address", text: $host)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .keyboardType(.URL)
            TextField("Port", text: $port)
                .keyboardType(.numberPad)
            Button("Connect") {
                let h = host.trimmingCharacters(in: .whitespaces)
                if !h.isEmpty { open(Target(host: h, port: UInt16(port) ?? 47800, id: nil, name: h)) }
            }
            Button("Cancel", role: .cancel) {}
        }
        .alert(forgetting.map { "Forget \($0.name)?" } ?? "", isPresented: Binding(get: { forgetting != nil }, set: { if !$0 { forgetting = nil } })) {
            Button("Forget", role: .destructive) {
                if let c = forgetting { connection.forget(c) }
                forgetting = nil
            }
            Button("Cancel", role: .cancel) { forgetting = nil }
        } message: {
            Text("Pairing again needs a new code from the computer.")
        }
    }

    private func row(_ computer: Computer) -> some View {
        let detail = connection.detail(computer)
        return Button {
            if connection.info?.id == computer.id {
                path = [.connected]
            } else if let target = computer.target {
                open(target)
            } else {
                connection.message = "\(computer.name) is not reachable."
            }
        } label: {
            VStack(alignment: .leading) {
                Text(computer.name)
                Text(detail).font(.caption).foregroundStyle(.secondary)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(computer.name), \(detail)")
        .accessibilityAddTraits(.isButton)
        .accessibilityActions {
            if computer.known != nil {
                Button("Forget") { forgetting = computer }
            }
        }
        .swipeActions {
            if computer.known != nil {
                Button("Forget", role: .destructive) { forgetting = computer }
            }
        }
    }

    private func open(_ target: Target) {
        connection.connect(target)
        path = [.connected]
    }
}
