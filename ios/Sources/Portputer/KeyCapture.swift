import SwiftUI
import UIKit

/// Takes the hardware keyboard's key presses while `active` and sends them to
/// the computer. iOS delivers keys only to the app in front, and keeps its
/// own shortcuts (such as Command-Tab) and, while VoiceOver is on,
/// VoiceOver's keyboard commands.
struct KeyCapture: UIViewRepresentable {
    var active: Bool

    func makeUIView(context: Context) -> KeyCatcherView {
        KeyCatcherView()
    }

    func updateUIView(_ view: KeyCatcherView, context: Context) {
        view.active = active
    }
}

final class KeyCatcherView: UIView {
    /// Keys pressed on the computer, by HID usage, so each is released there.
    private var sent: [Int: String] = [:]
    /// iOS doesn't repeat key presses, so held keys are repeated here.
    private var repeatTimer: Timer?
    /// A text field has the keyboard, as in Type Text or Pairing.
    private var editing = false
    /// Takes the keyboard back from whatever had it, such as a closed sheet.
    private var keepTimer: Timer?
    private var observers: [NSObjectProtocol] = []

    var active = false {
        didSet {
            guard active != oldValue else { return }
            if active {
                take()
                keepTimer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in self?.take() }
            } else {
                keepTimer?.invalidate()
                keepTimer = nil
                releaseAll()
                if isFirstResponder { _ = resignFirstResponder() }
            }
        }
    }

    init() {
        super.init(frame: .zero)
        isAccessibilityElement = false
        let center = NotificationCenter.default
        for name in [UITextField.textDidBeginEditingNotification, UITextView.textDidBeginEditingNotification] {
            observers.append(center.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in self?.editing = true })
        }
        for name in [UITextField.textDidEndEditingNotification, UITextView.textDidEndEditingNotification] {
            observers.append(center.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                self?.editing = false
                self?.take()
            })
        }
        observers.append(center.addObserver(forName: UIApplication.didBecomeActiveNotification, object: nil, queue: .main) { [weak self] _ in self?.take() })
        observers.append(center.addObserver(forName: UIApplication.willResignActiveNotification, object: nil, queue: .main) { [weak self] _ in self?.releaseAll() })
    }

    required init?(coder: NSCoder) { fatalError() }

    deinit {
        observers.forEach(NotificationCenter.default.removeObserver)
        keepTimer?.invalidate()
        repeatTimer?.invalidate()
    }

    override var canBecomeFirstResponder: Bool { active }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        take()
    }

    override func resignFirstResponder() -> Bool {
        releaseAll()
        return super.resignFirstResponder()
    }

    private func take() {
        guard active, !editing, window != nil, !isFirstResponder else { return }
        becomeFirstResponder()
    }

    private func releaseAll() {
        stopRepeat()
        guard !sent.isEmpty else { return }
        for code in sent.values { Connection.shared.key(code, down: false, repeat: false) }
        sent = [:]
    }

    override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        guard active else { return super.pressesBegan(presses, with: event) }
        var others = Set<UIPress>()
        for press in presses {
            guard let key = press.key else {
                others.insert(press)
                continue
            }
            let usage = key.keyCode.rawValue
            // Keys with no code on the computer are dropped.
            guard let code = codeForIosKey(hidUsage: UInt16(clamping: usage), characters: key.charactersIgnoringModifiers, byPosition: Settings.byPosition) else { continue }
            sent[usage] = code
            Connection.shared.key(code, down: true, repeat: false)
            if isModifierCode(code: code) {
                stopRepeat()
            } else {
                startRepeat(code)
            }
        }
        if !others.isEmpty { super.pressesBegan(others, with: event) }
    }

    override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        let others = release(presses)
        if !others.isEmpty { super.pressesEnded(others, with: event) }
    }

    override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        let others = release(presses)
        if !others.isEmpty { super.pressesCancelled(others, with: event) }
    }

    /// Releases the presses that were sent; returns the rest.
    private func release(_ presses: Set<UIPress>) -> Set<UIPress> {
        var others = Set<UIPress>()
        for press in presses {
            guard let usage = press.key?.keyCode.rawValue, let code = sent.removeValue(forKey: usage) else {
                others.insert(press)
                continue
            }
            if repeating == code { stopRepeat() }
            Connection.shared.key(code, down: false, repeat: false)
        }
        return others
    }

    private var repeating: String?

    /// The last key pressed repeats after 400 ms, 20 times a second, as on
    /// most keyboards.
    private func startRepeat(_ code: String) {
        stopRepeat()
        repeating = code
        repeatTimer = Timer.scheduledTimer(withTimeInterval: 0.4, repeats: false) { [weak self] _ in
            self?.repeatTimer = Timer.scheduledTimer(withTimeInterval: 0.05, repeats: true) { _ in
                MainActor.assumeIsolated { Connection.shared.key(code, down: true, repeat: true) }
            }
        }
    }

    private func stopRepeat() {
        repeatTimer?.invalidate()
        repeatTimer = nil
        repeating = nil
    }
}
