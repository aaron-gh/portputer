package io.github.aaron_gh.portputer

import android.app.Activity
import android.os.Bundle
import android.view.LayoutInflater
import android.view.MenuItem
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.TextView
import androidx.core.view.ViewCompat
import androidx.recyclerview.widget.LinearLayoutManager
import androidx.recyclerview.widget.RecyclerView
import uniffi.portputer_ffi.MenuKey
import uniffi.portputer_ffi.keyMenu

/**
 * Keys a keyboard may not have, and shortcuts, for the connected computer's
 * system. Activating a row presses the key; modifiers can also be held,
 * through a custom action or the Hold button beside them.
 */
class KeysActivity : Activity(), Connection.Listener {
    private sealed interface Item {
        data class Heading(val title: String) : Item
        data class Key(val key: MenuKey) : Item
    }

    private val items = mutableListOf<Item>()
    private val adapter = Adapter()
    private lateinit var status: TextView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_keys)
        Ui.toolbar(this, getString(R.string.keys_title), up = true)
        status = findViewById(R.id.status)
        val info = Connection.info
        if (info == null) {
            finish()
            return
        }
        status.text = getString(R.string.keys_target, info.name)
        for (section in keyMenu(info.os)) {
            items.add(Item.Heading(section.title))
            section.keys.forEach { items.add(Item.Key(it)) }
        }
        findViewById<RecyclerView>(R.id.list).apply {
            layoutManager = LinearLayoutManager(this@KeysActivity)
            adapter = this@KeysActivity.adapter
        }
        findViewById<Button>(R.id.release_all).setOnClickListener {
            Connection.releaseAll()
            status.text = getString(R.string.released_all)
        }
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
        if (Connection.info == null) finish() else adapter.notifyDataSetChanged()
    }

    override fun onOptionsItemSelected(item: MenuItem): Boolean {
        if (item.itemId == android.R.id.home) {
            finish()
            return true
        }
        return super.onOptionsItemSelected(item)
    }

    private fun press(key: MenuKey) {
        Connection.tap(key.code, key.modifiers)
        status.text = getString(R.string.key_pressed, key.label)
    }

    private fun toggleHold(key: MenuKey) {
        val holding = key.code !in Connection.held
        Connection.hold(key.code, holding)
        status.text = getString(if (holding) R.string.key_held else R.string.key_released, key.label)
    }

    private class HeadingHolder(view: View) : RecyclerView.ViewHolder(view) {
        val text: TextView = view.findViewById(R.id.heading)
    }

    private class KeyHolder(view: View) : RecyclerView.ViewHolder(view) {
        val row: View = view.findViewById(R.id.row)
        val label: TextView = view.findViewById(R.id.label)
        val hold: Button = view.findViewById(R.id.hold)
    }

    private inner class Adapter : RecyclerView.Adapter<RecyclerView.ViewHolder>() {
        override fun getItemCount() = items.size

        override fun getItemViewType(position: Int) = if (items[position] is Item.Heading) 0 else 1

        override fun onCreateViewHolder(parent: ViewGroup, viewType: Int): RecyclerView.ViewHolder {
            val inflater = LayoutInflater.from(parent.context)
            return if (viewType == 0) {
                HeadingHolder(inflater.inflate(R.layout.item_heading, parent, false))
            } else {
                KeyHolder(inflater.inflate(R.layout.item_key, parent, false))
            }
        }

        override fun onBindViewHolder(holder: RecyclerView.ViewHolder, position: Int) {
            when (val item = items[position]) {
                is Item.Heading -> (holder as HeadingHolder).text.text = item.title
                is Item.Key -> bindKey(holder as KeyHolder, item.key)
            }
        }

        private fun bindKey(holder: KeyHolder, key: MenuKey) {
            val held = key.code in Connection.held
            holder.label.text = key.label
            holder.row.contentDescription = key.label
            ViewCompat.setStateDescription(holder.row, if (held) getString(R.string.held) else null)
            holder.row.setOnClickListener { press(key) }
            if (key.holdable) {
                val label = getString(if (held) R.string.release else R.string.hold)
                holder.hold.visibility = View.VISIBLE
                holder.hold.text = label
                holder.hold.setOnClickListener { toggleHold(key) }
                Ui.setActions(holder.row, listOf(label to { toggleHold(key) }))
            } else {
                holder.hold.visibility = View.GONE
                Ui.setActions(holder.row, emptyList())
            }
        }
    }
}
