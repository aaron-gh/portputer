package io.github.aaron_gh.portputer

import android.Manifest
import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.content.pm.PackageManager
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Bundle
import android.provider.Settings as SystemSettings
import android.view.LayoutInflater
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import androidx.recyclerview.widget.LinearLayoutManager
import androidx.recyclerview.widget.RecyclerView
import uniffi.portputer_ffi.Advertisement
import uniffi.portputer_ffi.Discovery
import uniffi.portputer_ffi.DiscoveryListener
import uniffi.portputer_ffi.KnownServer
import uniffi.portputer_ffi.PortputerException
import uniffi.portputer_ffi.Reachability
import uniffi.portputer_ffi.probeComputer
import uniffi.portputer_ffi.SessionType
import uniffi.portputer_ffi.forgetServer
import uniffi.portputer_ffi.knownServers
import uniffi.portputer_ffi.wake

/** A computer in the list: paired, found on the network, or both. */
data class ComputerRow(val id: String, val known: KnownServer?, val ads: List<Advertisement>) {
    val name: String get() = ads.firstOrNull()?.name ?: known?.name ?: id
    val os: String get() = ads.firstOrNull()?.os ?: known?.os ?: ""
    val userAd: Advertisement? get() = ads.firstOrNull { it.session == SessionType.USER }
    val loginAd: Advertisement? get() = ads.firstOrNull { it.session == SessionType.LOGIN_WINDOW }
    val macs: List<String> get() = (ads.flatMap { it.macs } + (known?.macs ?: emptyList())).distinct()

    /** The user session's server first, then the login window's, then where it was last. */
    fun target(): Target? {
        (userAd ?: loginAd)?.let { ad ->
            ad.addresses.firstOrNull()?.let { return Target(it, ad.port.toInt(), id, name) }
        }
        // The last address, then the relay, which the core tries after it.
        return known?.takeIf { it.host.isNotEmpty() || it.hasRelay }?.let { Target(it.host, it.port.toInt(), id, name) }
    }
}

class ComputersActivity : Activity(), Connection.Listener {
    /** How each paired computer not on this network can be reached, when known. */
    private val reach = HashMap<String, Reachability>()
    private val checker = android.os.Handler(android.os.Looper.getMainLooper())
    private var checking = false
    private val check = object : Runnable {
        override fun run() {
            probe()
            checker.postDelayed(this, 15_000)
        }
    }
    private val ads = LinkedHashMap<String, Advertisement>()
    private var discovery: Discovery? = null
    private var multicast: WifiManager.MulticastLock? = null
    private val adapter = Adapter()
    private lateinit var status: TextView
    private lateinit var empty: TextView
    private lateinit var keyboardOff: View

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_computers)
        Ui.toolbar(this, getString(R.string.app_name), up = false)
        status = findViewById(R.id.status)
        empty = findViewById(R.id.empty)
        keyboardOff = findViewById(R.id.keyboard_off_section)
        findViewById<RecyclerView>(R.id.list).apply {
            layoutManager = LinearLayoutManager(this@ComputersActivity)
            adapter = this@ComputersActivity.adapter
        }
        findViewById<Button>(R.id.add_by_address).setOnClickListener { addByAddress() }
        findViewById<Button>(R.id.settings).setOnClickListener { startActivity(Intent(this, SettingsActivity::class.java)) }
        findViewById<Button>(R.id.open_accessibility).setOnClickListener {
            startActivity(Intent(SystemSettings.ACTION_ACCESSIBILITY_SETTINGS))
        }
        if (Build.VERSION.SDK_INT >= 33 && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1)
        }
    }

    override fun onStart() {
        super.onStart()
        Connection.addListener(this)
        multicast = getSystemService(WifiManager::class.java)?.createMulticastLock("portputer:discovery")?.apply {
            setReferenceCounted(false)
            acquire()
        }
        ads.clear()
        discovery = try {
            Discovery.start(object : DiscoveryListener {
                override fun found(ad: Advertisement) = runOnUiThread {
                    ads[ad.fullname] = ad
                    refresh()
                }

                override fun lost(fullname: String) = runOnUiThread {
                    ads.remove(fullname)
                    refresh()
                }
            })
        } catch (e: PortputerException) {
            say(e.message ?: "")
            null
        }
        keyboardOff.visibility = if (KeyboardService.isEnabled(this)) View.GONE else View.VISIBLE
        refresh()
        checker.post(check)
    }

    override fun onStop() {
        checker.removeCallbacks(check)
        Connection.removeListener(this)
        discovery?.stop()
        discovery?.close()
        discovery = null
        multicast?.takeIf { it.isHeld }?.release()
        super.onStop()
    }

    override fun onConnectionChanged() = refresh()

    private fun say(text: String) {
        status.text = text
        status.visibility = if (text.isEmpty()) View.GONE else View.VISIBLE
    }

    private fun refresh() {
        val known = knownServers(Connection.storeDir).associateBy { it.id }
        val byId = ads.values.groupBy { it.id }
        val rows = (known.keys + byId.keys).distinct().map { id -> ComputerRow(id, known[id], byId[id] ?: emptyList()) }
            .sortedWith(compareBy({ it.known == null }, { it.name.lowercase() }))
        adapter.rows = rows
        adapter.notifyDataSetChanged()
        empty.visibility = if (rows.isEmpty()) View.VISIBLE else View.GONE
    }

    private fun detail(row: ComputerRow): String {
        val os = when (row.os) {
            "mac" -> getString(R.string.os_mac)
            "windows" -> getString(R.string.os_windows)
            else -> getString(R.string.os_other)
        }
        val connectedHere = Connection.info?.id == row.id
        val state = when {
            connectedHere -> getString(R.string.status_connected)
            row.userAd != null -> row.userAd!!.user.takeIf { it.isNotEmpty() }?.let { getString(R.string.status_available_user, it) }
                ?: getString(R.string.status_available)
            row.loginAd != null -> getString(R.string.status_login_window)
            row.known != null -> when (val r = reach[row.id]) {
                null -> getString(R.string.status_checking)
                is Reachability.Direct -> getString(if (r.tailscale) R.string.status_tailscale else R.string.status_other_network)
                is Reachability.Relay -> getString(R.string.status_relay)
                is Reachability.NotReachable -> getString(R.string.status_offline)
            }
            else -> getString(R.string.status_offline)
        }
        val parts = mutableListOf(os, state)
        if (row.known == null) parts.add(getString(R.string.status_not_paired))
        return parts.joinToString(", ")
    }

    /** Checks the paired computers that discovery doesn't see here. */
    private fun probe() {
        if (checking) return
        val ids = adapter.rows.filter { it.known != null && it.ads.isEmpty() && Connection.info?.id != it.id }.map { it.id }
        if (ids.isEmpty()) return
        checking = true
        kotlin.concurrent.thread(name = "portputer-probe") {
            val results = ids.associateWith { probeComputer(Connection.storeDir, it) }
            runOnUiThread {
                checking = false
                if (results.any { (id, r) -> reach[id] != r }) {
                    reach.putAll(results)
                    refresh()
                }
            }
        }
    }

    private fun open(row: ComputerRow) {
        val target = row.target() ?: return say(getString(R.string.status_offline))
        if (Connection.info?.id != row.id || Connection.state is Connection.State.Closed) {
            Connection.connect(target)
        }
        startActivity(Intent(this, ConnectedActivity::class.java))
    }

    private fun wakeComputer(row: ComputerRow) {
        say(getString(if (wake(row.macs)) R.string.wake_sent else R.string.wake_failed, row.name))
    }

    private fun forget(row: ComputerRow) {
        AlertDialog.Builder(this)
            .setTitle(getString(R.string.forget_title, row.name))
            .setMessage(R.string.forget_message)
            .setPositiveButton(R.string.forget) { _, _ ->
                if (Connection.info?.id == row.id) Connection.disconnect()
                runCatching { forgetServer(Connection.storeDir, row.id) }
                say(getString(R.string.forgotten, row.name))
                refresh()
            }
            .setNegativeButton(R.string.cancel, null)
            .show()
    }

    private fun addByAddress() {
        val view = layoutInflater.inflate(R.layout.dialog_address, null)
        val host = view.findViewById<EditText>(R.id.host)
        val port = view.findViewById<EditText>(R.id.port)
        AlertDialog.Builder(this)
            .setTitle(R.string.address_title)
            .setView(view)
            .setPositiveButton(R.string.connect) { _, _ ->
                val h = host.text.toString().trim()
                val p = port.text.toString().toIntOrNull() ?: 47800
                if (h.isNotEmpty()) {
                    Connection.connect(Target(h, p, null, h))
                    startActivity(Intent(this, ConnectedActivity::class.java))
                }
            }
            .setNegativeButton(R.string.cancel, null)
            .show()
    }

    private inner class Holder(view: View) : RecyclerView.ViewHolder(view) {
        val row: View = view.findViewById(R.id.row)
        val name: TextView = view.findViewById(R.id.name)
        val detail: TextView = view.findViewById(R.id.detail)
        val wake: Button = view.findViewById(R.id.wake)
        val forget: Button = view.findViewById(R.id.forget)
    }

    private inner class Adapter : RecyclerView.Adapter<Holder>() {
        var rows: List<ComputerRow> = emptyList()

        override fun getItemCount() = rows.size

        override fun onCreateViewHolder(parent: ViewGroup, viewType: Int) =
            Holder(LayoutInflater.from(parent.context).inflate(R.layout.item_computer, parent, false))

        override fun onBindViewHolder(holder: Holder, position: Int) {
            val row = rows[position]
            val detailText = detail(row)
            holder.name.text = row.name
            holder.detail.text = detailText
            holder.row.contentDescription = "${row.name}, $detailText"
            holder.row.setOnClickListener { open(row) }
            Ui.clickLabel(holder.row, getString(R.string.connect))
            val canWake = row.ads.isEmpty() && row.macs.isNotEmpty() && reach[row.id] !is Reachability.Direct && reach[row.id] !is Reachability.Relay
            holder.wake.visibility = if (canWake) View.VISIBLE else View.GONE
            holder.wake.setOnClickListener { wakeComputer(row) }
            holder.forget.visibility = if (row.known != null) View.VISIBLE else View.GONE
            holder.forget.setOnClickListener { forget(row) }
            val actions = mutableListOf<Pair<String, () -> Unit>>()
            if (canWake) actions.add(getString(R.string.wake) to { wakeComputer(row) })
            if (row.known != null) actions.add(getString(R.string.forget) to { forget(row) })
            Ui.setActions(holder.row, actions)
        }
    }
}
