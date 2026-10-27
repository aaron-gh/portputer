package io.github.aaron_gh.portputer

import android.content.Context
import android.os.Build
import android.view.KeyEvent

/** The key combination that switches the keyboard between the computer and this device. */
enum class ReleaseShortcut(val label: Int) {
    CTRL_META_ESC(R.string.release_ctrl_meta_esc),
    CTRL_ALT_SHIFT_ESC(R.string.release_ctrl_alt_shift_esc),
    META_ESC(R.string.release_meta_esc),
    CTRL_SHIFT_F12(R.string.release_ctrl_shift_f12),
    SCROLL_LOCK(R.string.release_scroll_lock),
    PAUSE(R.string.release_pause);

    fun matches(e: KeyEvent): Boolean = when (this) {
        CTRL_META_ESC -> e.keyCode == KeyEvent.KEYCODE_ESCAPE && e.isCtrlPressed && e.isMetaPressed
        CTRL_ALT_SHIFT_ESC -> e.keyCode == KeyEvent.KEYCODE_ESCAPE && e.isCtrlPressed && e.isAltPressed && e.isShiftPressed
        META_ESC -> e.keyCode == KeyEvent.KEYCODE_ESCAPE && e.isMetaPressed
        CTRL_SHIFT_F12 -> e.keyCode == KeyEvent.KEYCODE_F12 && e.isCtrlPressed && e.isShiftPressed
        SCROLL_LOCK -> e.keyCode == KeyEvent.KEYCODE_SCROLL_LOCK
        PAUSE -> e.keyCode == KeyEvent.KEYCODE_BREAK
    }
}

class Settings(context: Context) {
    private val prefs = context.applicationContext.getSharedPreferences("settings", Context.MODE_PRIVATE)

    var deviceName: String
        get() = prefs.getString("device_name", null)?.takeIf { it.isNotBlank() } ?: Build.MODEL
        set(value) = prefs.edit().putString("device_name", value.trim()).apply()

    /** Keys by position (the scan code), so the computer's layout applies, or by this device's layout. */
    var byPosition: Boolean
        get() = prefs.getBoolean("by_position", true)
        set(value) = prefs.edit().putBoolean("by_position", value).apply()

    var release: ReleaseShortcut
        get() = runCatching { ReleaseShortcut.valueOf(prefs.getString("release", "")!!) }.getOrDefault(ReleaseShortcut.CTRL_META_ESC)
        set(value) = prefs.edit().putString("release", value.name).apply()

    var bufferMs: Int
        get() = prefs.getInt("buffer_ms", 40)
        set(value) = prefs.edit().putInt("buffer_ms", value).apply()

    var volume: Int
        get() = prefs.getInt("volume", 100)
        set(value) = prefs.edit().putInt("volume", value).apply()

    var captureOnConnect: Boolean
        get() = prefs.getBoolean("capture_on_connect", true)
        set(value) = prefs.edit().putBoolean("capture_on_connect", value).apply()

    var audioOnConnect: Boolean
        get() = prefs.getBoolean("audio_on_connect", true)
        set(value) = prefs.edit().putBoolean("audio_on_connect", value).apply()

    var haptics: Boolean
        get() = prefs.getBoolean("haptics", true)
        set(value) = prefs.edit().putBoolean("haptics", value).apply()

    var builtinKeyboards: Boolean
        get() = prefs.getBoolean("builtin_keyboards", false)
        set(value) = prefs.edit().putBoolean("builtin_keyboards", value).apply()

    companion object {
        val BUFFERS = listOf(20, 40, 60, 100, 150, 250)
    }
}
