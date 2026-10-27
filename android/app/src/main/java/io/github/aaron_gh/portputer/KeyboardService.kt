package io.github.aaron_gh.portputer

import android.accessibilityservice.AccessibilityService
import android.content.ComponentName
import android.content.Context
import android.os.Build
import android.provider.Settings as SystemSettings
import android.view.InputDevice
import android.view.KeyEvent
import android.view.accessibility.AccessibilityEvent
import uniffi.portputer_ffi.codeForAndroidKey

/**
 * Takes key events from external keyboards before anything else on the
 * device, and while the keyboard is sent to the computer, passes every one
 * there instead. The release shortcut switches between the two.
 */
class KeyboardService : AccessibilityService() {
    companion object {
        @Volatile var running = false
            private set

        fun isEnabled(context: Context): Boolean {
            if (running) return true
            val enabled = SystemSettings.Secure.getString(
                context.contentResolver, SystemSettings.Secure.ENABLED_ACCESSIBILITY_SERVICES,
            ) ?: return false
            val me = ComponentName(context, KeyboardService::class.java)
            return enabled.split(':').any { ComponentName.unflattenFromString(it) == me }
        }
    }

    private lateinit var settings: Settings
    /** Keys whose release is swallowed: the release shortcut's. */
    private val swallowed = HashSet<Int>()
    /** Keys pressed on the computer, by Android key code, so each is released there. */
    private val sent = HashMap<Int, String>()

    override fun onServiceConnected() {
        settings = Settings(this)
        running = true
    }

    override fun onDestroy() {
        running = false
        super.onDestroy()
    }

    override fun onAccessibilityEvent(event: AccessibilityEvent?) {}

    override fun onInterrupt() {}

    private fun fromKeyboard(event: KeyEvent): Boolean {
        val device = event.device ?: return false
        if (device.isVirtual) return false
        if (device.sources and InputDevice.SOURCE_KEYBOARD != InputDevice.SOURCE_KEYBOARD) return false
        val external = if (Build.VERSION.SDK_INT >= 29) device.isExternal else device.keyboardType == InputDevice.KEYBOARD_TYPE_ALPHABETIC
        return external || (settings.builtinKeyboards && device.keyboardType == InputDevice.KEYBOARD_TYPE_ALPHABETIC)
    }

    override fun onKeyEvent(event: KeyEvent): Boolean {
        if (!fromKeyboard(event)) return false
        val down = event.action == KeyEvent.ACTION_DOWN
        if (!down && swallowed.remove(event.keyCode)) return true
        if (!down) {
            sent.remove(event.keyCode)?.let {
                Connection.key(it, false, false)
                return true
            }
            return Connection.capturing && Connection.canSend()
        }
        if (!Connection.canSend()) return false
        if (settings.release.matches(event)) {
            swallowed.add(event.keyCode)
            if (event.repeatCount == 0) Connection.toggleCapture(this)
            return true
        }
        if (!Connection.capturing) return false
        val code = codeForAndroidKey(event.scanCode.toUInt(), event.keyCode.toUInt(), settings.byPosition) ?: return true
        sent[event.keyCode] = code
        Connection.key(code, true, event.repeatCount > 0)
        return true
    }
}
