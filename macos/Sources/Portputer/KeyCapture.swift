import AppKit
import SwiftUI

// CoreGraphics' private calls for switching system shortcuts off, as AAE,
// Parallels and VMware use. The mode lasts only while this process runs.
@_silgen_name("CGSMainConnectionID")
private func CGSMainConnectionID() -> Int32
@_silgen_name("CGSSetGlobalHotKeyOperatingMode")
private func CGSSetGlobalHotKeyOperatingMode(_ connection: Int32, _ mode: Int32) -> Int32
private let hotKeysEnabled: Int32 = 0
/// Every system shortcut off except Universal Access ones, such as VoiceOver's.
private let hotKeysExceptUniversalAccess: Int32 = 2

/// The view that holds the keyboard while it's sent to the computer,
/// forwarding every key by its macOS key code. As in AAE: Command
/// combinations arrive through performKeyEquivalent and never get a key-up,
/// so they're sent as taps; a key held while Command is down never gets its
/// key-up either, so held keys are released when a modifier is; modifiers
/// arrive as flagsChanged, read from their device-specific flags.
final class KeyCaptureView: NSView {
    var send: ((UInt16, Bool, Bool) -> Void)?
    var onEscape: (() -> Void)?

    var isCapturing = false {
        didSet {
            if isCapturing {
                window?.makeFirstResponder(self)
            } else {
                releaseAll()
            }
        }
    }

    private var heldKeys = Set<UInt16>()
    private var heldModifiers = Set<UInt16>()

    private static let modifierMasks: [UInt16: UInt] = [
        0x37: 0x0008, 0x36: 0x0010, // Command
        0x38: 0x0002, 0x3C: 0x0004, // Shift
        0x3B: 0x0001, 0x3E: 0x2000, // Control
        0x3A: 0x0020, 0x3D: 0x0040, // Option
    ]
    private static let capsLock: UInt16 = 0x39
    private static let function: UInt16 = 0x3F

    /// Control-Command-Escape gives the keyboard back to the Mac.
    static func isEscape(_ event: NSEvent) -> Bool {
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        return event.keyCode == 0x35 && flags.contains([.control, .command])
    }

    override var acceptsFirstResponder: Bool { isCapturing }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
        setAccessibilityLabel("Keyboard to computer")
    }

    override func keyDown(with event: NSEvent) {
        guard isCapturing else { return super.keyDown(with: event) }
        if Self.isEscape(event) { return onEscape?() ?? () }
        heldKeys.insert(event.keyCode)
        send?(event.keyCode, true, event.isARepeat)
    }

    override func keyUp(with event: NSEvent) {
        guard isCapturing else { return super.keyUp(with: event) }
        heldKeys.remove(event.keyCode)
        send?(event.keyCode, false, false)
    }

    override func flagsChanged(with event: NSEvent) {
        guard isCapturing else { return super.flagsChanged(with: event) }
        let code = event.keyCode
        if code == Self.capsLock {
            send?(code, true, false)
            send?(code, false, false)
            return
        }
        if code == Self.function {
            send?(code, event.modifierFlags.contains(.function), false)
            return
        }
        guard let mask = Self.modifierMasks[code] else { return }
        if event.modifierFlags.rawValue & mask != 0 {
            heldModifiers.insert(code)
            send?(code, true, false)
        } else {
            heldModifiers.remove(code)
            releaseHeldKeys()
            send?(code, false, false)
        }
    }

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        if isCapturing, event.type == .keyDown, Self.isEscape(event) {
            onEscape?()
            return true
        }
        guard isCapturing, event.type == .keyDown, event.modifierFlags.contains(.command) else { return false }
        if !event.isARepeat {
            send?(event.keyCode, true, false)
            send?(event.keyCode, false, false)
        }
        return true
    }

    func releaseAll() {
        releaseHeldKeys()
        for code in heldModifiers { send?(code, false, false) }
        heldModifiers.removeAll()
    }

    private func releaseHeldKeys() {
        for code in heldKeys { send?(code, false, false) }
        heldKeys.removeAll()
    }
}

/// While the keyboard is sent to the computer: system shortcuts off (so
/// Command-Tab and Command-Space reach it), app switching blocked, and
/// Control-Command-Escape caught before anything else in the app.
@MainActor
final class KeyboardLock {
    static let shared = KeyboardLock()
    private var monitor: Any?
    private var tokens: [NSObjectProtocol] = []
    private(set) var isLocked = false

    func lock(onEscape: @escaping () -> Void) {
        guard !isLocked else { return }
        isLocked = true
        NSApp.presentationOptions = [.disableProcessSwitching, .disableForceQuit, .disableHideApplication]
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { event in
            guard KeyCaptureView.isEscape(event) else { return event }
            onEscape()
            return nil
        }
        setShortcuts(captured: NSApp.isActive)
        let center = NotificationCenter.default
        tokens = [
            center.addObserver(forName: NSApplication.didBecomeActiveNotification, object: nil, queue: .main) { _ in
                MainActor.assumeIsolated { KeyboardLock.shared.setShortcuts(captured: true) }
            },
            center.addObserver(forName: NSApplication.didResignActiveNotification, object: nil, queue: .main) { _ in
                MainActor.assumeIsolated { KeyboardLock.shared.setShortcuts(captured: false) }
            },
        ]
    }

    func unlock() {
        guard isLocked else { return }
        isLocked = false
        NSApp.presentationOptions = []
        if let monitor { NSEvent.removeMonitor(monitor) }
        monitor = nil
        tokens.forEach(NotificationCenter.default.removeObserver)
        tokens = []
        setShortcuts(captured: false)
    }

    private func setShortcuts(captured: Bool) {
        _ = CGSSetGlobalHotKeyOperatingMode(CGSMainConnectionID(), captured && isLocked ? hotKeysExceptUniversalAccess : hotKeysEnabled)
    }
}

struct KeyCapture: NSViewRepresentable {
    var capturing: Bool
    var send: (UInt16, Bool, Bool) -> Void
    var onEscape: () -> Void

    func makeNSView(context: Context) -> KeyCaptureView {
        let view = KeyCaptureView()
        view.send = send
        view.onEscape = onEscape
        return view
    }

    func updateNSView(_ view: KeyCaptureView, context: Context) {
        view.send = send
        view.onEscape = onEscape
        if view.isCapturing != capturing {
            view.isCapturing = capturing
        }
    }
}
