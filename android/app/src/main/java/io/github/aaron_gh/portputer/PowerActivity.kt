package io.github.aaron_gh.portputer

import android.app.Activity
import android.app.AlertDialog
import android.os.Bundle
import android.view.MenuItem
import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import uniffi.portputer_ffi.Power

/** Restart, shut down and the rest, for the actions the computer offers. */
class PowerActivity : Activity(), Connection.Listener {
    private lateinit var status: TextView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_power)
        Ui.toolbar(this, getString(R.string.power_title), up = true)
        status = findViewById(R.id.status)
        val info = Connection.info ?: return finish()
        val mac = info.os == "mac"
        val buttons = mapOf(
            Power.RESTART to listOf(R.id.restart, R.id.restart_help),
            Power.SHUTDOWN to listOf(R.id.shutdown),
            Power.SLEEP to listOf(R.id.sleep),
            Power.LOCK to listOf(R.id.lock),
            Power.LOG_OUT to listOf(R.id.log_out),
            Power.FORCE_RESTART to listOf(R.id.force_restart, R.id.force_restart_help),
            Power.UNLOCKED_RESTART to listOf(R.id.unlocked_restart, R.id.unlocked_restart_help),
        )
        for ((action, ids) in buttons) {
            val shown = action in info.power
            ids.forEach { findViewById<View>(it).visibility = if (shown) View.VISIBLE else View.GONE }
            findViewById<Button>(ids[0]).setOnClickListener { choose(action, (it as Button).text.toString()) }
        }
        if (!mac) findViewById<Button>(R.id.unlocked_restart).setText(R.string.power_unlocked_restart)
        if (mac && Power.FORCE_RESTART !in info.power) {
            findViewById<View>(R.id.no_system_components).visibility = View.VISIBLE
        }
        status.text = if (info.power.isEmpty()) getString(R.string.power_none) else info.name
    }

    override fun onStart() {
        super.onStart()
        Connection.addListener(this)
    }

    override fun onStop() {
        Connection.removeListener(this)
        super.onStop()
    }

    override fun onConnectionChanged() {
        if (Connection.info == null) finish()
    }

    override fun onMessage(text: String) {
        status.text = text
    }

    override fun onOptionsItemSelected(item: MenuItem): Boolean {
        if (item.itemId == android.R.id.home) {
            finish()
            return true
        }
        return super.onOptionsItemSelected(item)
    }

    private fun send(action: Power, label: String, user: String? = null, password: String? = null) {
        val name = Connection.info?.name ?: return
        Connection.power(action, user, password)
        status.text = getString(R.string.power_sent, label, name)
    }

    private fun choose(action: Power, label: String) {
        val name = Connection.info?.name ?: return
        when (action) {
            Power.LOCK -> send(action, label)
            Power.UNLOCKED_RESTART -> credentials(action, label)
            else -> AlertDialog.Builder(this)
                .setTitle(getString(R.string.power_confirm, label, name))
                .setMessage(
                    when (action) {
                        Power.RESTART -> getString(R.string.power_restart_help)
                        Power.FORCE_RESTART -> getString(R.string.power_force_restart_help)
                        else -> null
                    },
                )
                .setPositiveButton(label) { _, _ -> send(action, label) }
                .setNegativeButton(R.string.cancel, null)
                .show()
        }
    }

    private fun credentials(action: Power, label: String) {
        val info = Connection.info ?: return
        val view = layoutInflater.inflate(R.layout.dialog_credentials, null)
        val user = view.findViewById<EditText>(R.id.user)
        val password = view.findViewById<EditText>(R.id.password)
        user.setText(info.user)
        AlertDialog.Builder(this)
            .setTitle(getString(R.string.power_confirm, label, info.name))
            .setView(view)
            .setPositiveButton(label) { _, _ ->
                send(action, label, user.text.toString().trim(), password.text.toString())
            }
            .setNegativeButton(R.string.cancel, null)
            .show()
    }
}
