import Foundation

/// Finds servers on the local network through the system's Bonjour, as
/// `_portputer._tcp`. The Rust core's own mDNS needs multicast sockets, which
/// iOS allows only with an entitlement. What's found is also passed to the
/// core, for reconnecting at new addresses.
final class Bonjour: NSObject, NetServiceBrowserDelegate, NetServiceDelegate {
    private let browser = NetServiceBrowser()
    private var services: [NetService] = []
    private let found: (Advertisement) -> Void
    private let lost: (String) -> Void

    init(found: @escaping (Advertisement) -> Void, lost: @escaping (String) -> Void) {
        self.found = found
        self.lost = lost
        super.init()
        browser.delegate = self
    }

    func start() {
        browser.searchForServices(ofType: "_portputer._tcp.", inDomain: "local.")
    }

    func stop() {
        browser.stop()
        services.forEach { $0.stop() }
        services = []
    }

    private static func fullname(_ s: NetService) -> String {
        "\(s.name).\(s.type)\(s.domain)"
    }

    func netServiceBrowser(_ browser: NetServiceBrowser, didFind service: NetService, moreComing: Bool) {
        services.append(service)
        service.delegate = self
        service.resolve(withTimeout: 10)
        service.startMonitoring()
    }

    func netServiceBrowser(_ browser: NetServiceBrowser, didRemove service: NetService, moreComing: Bool) {
        services.removeAll { $0 === service }
        service.stop()
        let name = Self.fullname(service)
        networkLost(fullname: name)
        lost(name)
    }

    func netServiceDidResolveAddress(_ sender: NetService) {
        report(sender)
    }

    func netService(_ sender: NetService, didUpdateTXTRecord data: Data) {
        report(sender)
    }

    private func report(_ s: NetService) {
        guard let data = s.txtRecordData(), let addresses = s.addresses, !addresses.isEmpty else { return }
        let txt = NetService.dictionary(fromTXTRecord: data).compactMapValues { String(data: $0, encoding: .utf8) }
        let ips = addresses.compactMap(Self.numericHost)
        if let ad = networkFound(fullname: Self.fullname(s), txt: txt, addresses: ips, port: UInt16(s.port)) {
            found(ad)
        }
    }

    /// The numeric address in a sockaddr.
    private static func numericHost(_ data: Data) -> String? {
        data.withUnsafeBytes { raw -> String? in
            guard let sa = raw.baseAddress?.assumingMemoryBound(to: sockaddr.self) else { return nil }
            var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
            guard getnameinfo(sa, socklen_t(data.count), &host, socklen_t(host.count), nil, 0, NI_NUMERICHOST) == 0 else { return nil }
            return String(cString: host)
        }
    }
}
