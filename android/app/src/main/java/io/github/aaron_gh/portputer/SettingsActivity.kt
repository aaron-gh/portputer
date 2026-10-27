package io.github.aaron_gh.portputer

import android.app.Activity
import android.os.Bundle
import android.view.MenuItem
import android.widget.EditText
import android.widget.RadioButton
import android.widget.RadioGroup
import android.widget.Switch
import android.widget.TextView

class SettingsActivity : Activity() {
    private lateinit var settings: Settings
    private lateinit var deviceName: EditText

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_settings)
        Ui.toolbar(this, getString(R.string.settings_title), up = true)
        settings = Settings(this)

        deviceName = findViewById(R.id.device_name)
        deviceName.setText(settings.deviceName)

        val mapping = findViewById<RadioGroup>(R.id.mapping)
        mapping.check(if (settings.byPosition) R.id.mapping_position else R.id.mapping_layout)
        mapping.setOnCheckedChangeListener { _, id -> settings.byPosition = id == R.id.mapping_position }

        val release = findViewById<RadioGroup>(R.id.release)
        for (shortcut in ReleaseShortcut.entries) {
            release.addView(radio(getString(shortcut.label), shortcut.ordinal, shortcut == settings.release))
        }
        release.setOnCheckedChangeListener { group, id ->
            group.findViewById<RadioButton>(id)?.let { settings.release = ReleaseShortcut.entries[it.tag as Int] }
        }

        val buffer = findViewById<RadioGroup>(R.id.buffer)
        for ((index, ms) in Settings.BUFFERS.withIndex()) {
            val label = if (ms == 0) getString(R.string.settings_buffer_auto) else getString(R.string.settings_buffer_ms, ms)
            buffer.addView(radio(label, index, ms == settings.bufferMs))
        }
        buffer.setOnCheckedChangeListener { group, id ->
            group.findViewById<RadioButton>(id)?.let {
                val ms = Settings.BUFFERS[it.tag as Int]
                settings.bufferMs = ms
                Connection.setBuffer(ms)
            }
        }

        val version = packageManager.getPackageInfo(packageName, 0).versionName ?: ""
        findViewById<TextView>(R.id.version).text = getString(R.string.settings_version, version)
        val updateStatus = findViewById<TextView>(R.id.update_status)
        findViewById<android.widget.Button>(R.id.check_updates).setOnClickListener {
            UpdatePrompt.check(this, quietly = false) { updateStatus.text = it }
        }

        bind(R.id.capture_on_connect, settings.captureOnConnect) { settings.captureOnConnect = it }
        bind(R.id.audio_on_connect, settings.audioOnConnect) { settings.audioOnConnect = it }
        bind(R.id.haptics, settings.haptics) { settings.haptics = it }
        bind(R.id.builtin_keyboards, settings.builtinKeyboards) { settings.builtinKeyboards = it }
    }

    private fun radio(label: String, index: Int, checked: Boolean) = RadioButton(this).apply {
        id = android.view.View.generateViewId()
        text = label
        tag = index
        minHeight = (48 * resources.displayMetrics.density).toInt()
        isChecked = checked
    }

    private fun bind(id: Int, value: Boolean, set: (Boolean) -> Unit) {
        findViewById<Switch>(id).apply {
            isChecked = value
            setOnCheckedChangeListener { _, on -> set(on) }
        }
    }

    override fun onPause() {
        settings.deviceName = deviceName.text.toString()
        super.onPause()
    }

    override fun onOptionsItemSelected(item: MenuItem): Boolean {
        if (item.itemId == android.R.id.home) {
            finish()
            return true
        }
        return super.onOptionsItemSelected(item)
    }
}
