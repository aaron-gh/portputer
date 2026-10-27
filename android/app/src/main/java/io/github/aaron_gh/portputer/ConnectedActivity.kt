package io.github.aaron_gh.portputer

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.os.Bundle
import android.provider.Settings as SystemSettings
import android.view.MenuItem
import android.view.View
import android.view.inputmethod.EditorInfo
import android.widget.Button
import android.widget.EditText
import android.widget.SeekBar
import android.widget.Switch
import android.widget.TextView
import uniffi.portputer_ffi.SessionType

/** The connected computer: pairing, the keyboard and audio switches, and what can be sent. */
class ConnectedActivity : Activity(), Connection.Listener {
    private lateinit var status: TextView
    private lateinit var message: TextView
    private lateinit var pairSection: View
    private lateinit var pairInstructions: TextView
    private lateinit var pairCode: EditText
    private lateinit var pairButton: Button
    private lateinit var controls: View
    private lateinit var capture: Switch
    private lateinit var captureHelp: TextView
    private lateinit var keyboardOff: View
    private lateinit var audio: Switch
    private lateinit var muteSpeakers: Switch
    private lateinit var volume: SeekBar
    private lateinit var power: Button
    private lateinit var disconnect: Button
    private var updating = false
    private var lastStatus = ""

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_connected)
        Ui.toolbar(this, getString(R.string.app_name), up = true)
        status = findViewById(R.id.status)
        message = findViewById(R.id.message)
        pairSection = findViewById(R.id.pair_section)
        pairInstructions = findViewById(R.id.pair_instructions)
        pairCode = findViewById(R.id.pair_code)
        pairButton = findViewById(R.id.pair)
        controls = findViewById(R.id.controls)
        capture = findViewById(R.id.capture)
        captureHelp = findViewById(R.id.capture_help)
        keyboardOff = findViewById(R.id.keyboard_off_section)
        audio = findViewById(R.id.audio)
        muteSpeakers = findViewById(R.id.mute_speakers)
        volume = findViewById(R.id.volume)
        power = findViewById(R.id.power)
        disconnect = findViewById(R.id.disconnect)

        pairButton.setOnClickListener { pair() }
        pairCode.setOnEditorActionListener { _, action, _ ->
            if (action == EditorInfo.IME_ACTION_DONE) {
                pair()
                true
            } else {
                false
            }
        }
        capture.setOnCheckedChangeListener { _, on -> if (!updating) Connection.setCapturing(on) }
        audio.setOnCheckedChangeListener { _, on -> if (!updating) Connection.setAudio(on) }
        muteSpeakers.setOnCheckedChangeListener { _, on -> if (!updating) Connection.setSpeakersMuted(on) }
        volume.progress = Settings(this).volume
        volume.setOnSeekBarChangeListener(object : SeekBar.OnSeekBarChangeListener {
            override fun onProgressChanged(bar: SeekBar, progress: Int, fromUser: Boolean) {
                if (fromUser) Connection.setVolume(progress)
            }

            override fun onStartTrackingTouch(bar: SeekBar) {}
            override fun onStopTrackingTouch(bar: SeekBar) {}
        })
        findViewById<Button>(R.id.open_accessibility).setOnClickListener {
            startActivity(Intent(SystemSettings.ACTION_ACCESSIBILITY_SETTINGS))
        }
        findViewById<Button>(R.id.keys).setOnClickListener { startActivity(Intent(this, KeysActivity::class.java)) }
        findViewById<Button>(R.id.text).setOnClickListener { typeText() }
        power.setOnClickListener { startActivity(Intent(this, PowerActivity::class.java)) }
        disconnect.setOnClickListener {
            if (Connection.state is Connection.State.Closed || Connection.state is Connection.State.Idle) {
                finish()
            } else {
                Connection.disconnect()
                finish()
            }
        }
    }

    override fun onStart() {
        super.onStart()
        Connection.addListener(this)
        render()
    }

    override fun onStop() {
        Connection.removeListener(this)
        super.onStop()
    }

    override fun onOptionsItemSelected(item: MenuItem): Boolean {
        if (item.itemId == android.R.id.home) {
            finish()
            return true
        }
        return super.onOptionsItemSelected(item)
    }

    override fun onConnectionChanged() = render()

    override fun onMessage(text: String) {
        message.text = text
        message.visibility = View.VISIBLE
    }

    private fun setStatus(text: String) {
        // Set only when it changes, so the live region doesn't repeat itself.
        if (text != lastStatus) {
            lastStatus = text
            status.text = text
        }
    }

    private fun render() {
        updating = true
        when (val state = Connection.state) {
            is Connection.State.Idle -> {
                updating = false
                finish()
                return
            }
            is Connection.State.Connecting -> {
                title = state.target.name
                setStatus(getString(R.string.connecting, state.target.name))
                pairSection.visibility = View.GONE
                controls.visibility = View.GONE
                disconnect.setText(R.string.disconnect)
            }
            is Connection.State.Connected -> {
                val info = state.info
                title = info.name
                setStatus(
                    when {
                        info.session == SessionType.LOGIN_WINDOW -> getString(R.string.connected_login_window, info.name)
                        info.user.isNotEmpty() -> getString(R.string.connected_user, info.name, info.user)
                        else -> getString(R.string.connected_nobody, info.name)
                    },
                )
                disconnect.setText(R.string.disconnect)
                if (!info.trusted) {
                    pairInstructions.text = getString(R.string.pair_instructions, info.name)
                    pairSection.visibility = View.VISIBLE
                    controls.visibility = View.GONE
                } else {
                    pairSection.visibility = View.GONE
                    controls.visibility = View.VISIBLE
                    setControlsEnabled(true)
                    renderControls(info.name, info.audio, info.mute, info.power.isNotEmpty())
                }
            }
            is Connection.State.Reconnecting -> {
                setStatus(getString(R.string.reconnecting, state.info.name))
                setControlsEnabled(false)
            }
            is Connection.State.Closed -> {
                setStatus(state.reason)
                pairSection.visibility = View.GONE
                controls.visibility = View.GONE
                disconnect.setText(R.string.back_to_computers)
            }
        }
        updating = false
    }

    private fun renderControls(name: String, hasAudio: Boolean, canMute: Boolean, hasPower: Boolean) {
        val serviceOn = KeyboardService.isEnabled(this)
        keyboardOff.visibility = if (serviceOn) View.GONE else View.VISIBLE
        capture.isEnabled = serviceOn
        capture.isChecked = Connection.capturing
        val shortcut = getString(Settings(this).release.label)
        captureHelp.text = if (Connection.capturing) {
            getString(R.string.capture_on, name, shortcut)
        } else {
            getString(R.string.capture_off, shortcut, name)
        }
        audio.isEnabled = hasAudio
        audio.isChecked = Connection.audioWanted
        volume.isEnabled = hasAudio
        muteSpeakers.visibility = if (hasAudio && canMute) View.VISIBLE else View.GONE
        muteSpeakers.isChecked = Settings(this).muteSpeakers
        power.visibility = if (hasPower) View.VISIBLE else View.GONE
    }

    private fun setControlsEnabled(enabled: Boolean) {
        listOf<View>(capture, audio, muteSpeakers, volume, power, findViewById(R.id.keys), findViewById(R.id.text)).forEach { it.isEnabled = enabled }
    }

    private fun pair() {
        val code = pairCode.text.toString().trim()
        if (code.isEmpty()) return
        pairButton.isEnabled = false
        setStatus(getString(R.string.pairing))
        Connection.pair(code) { error ->
            pairButton.isEnabled = true
            if (error == null) {
                pairCode.text.clear()
                onMessage(getString(R.string.paired, Connection.info?.name ?: ""))
                lastStatus = ""
                render()
            } else {
                onMessage(getString(R.string.pair_failed, error))
                lastStatus = ""
                render()
            }
        }
    }

    private fun typeText() {
        val info = Connection.info ?: return
        val view = layoutInflater.inflate(R.layout.dialog_text, null)
        val field = view.findViewById<EditText>(R.id.text)
        AlertDialog.Builder(this)
            .setTitle(getString(R.string.text_title, info.name))
            .setView(view)
            .setPositiveButton(R.string.text_send) { _, _ ->
                val text = field.text.toString()
                if (text.isNotEmpty()) {
                    Connection.text(text)
                    onMessage(getString(R.string.text_sent))
                }
            }
            .setNegativeButton(R.string.cancel, null)
            .show()
    }
}
