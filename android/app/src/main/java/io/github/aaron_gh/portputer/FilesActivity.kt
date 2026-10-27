package io.github.aaron_gh.portputer

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.text.format.DateUtils
import android.view.LayoutInflater
import android.view.MenuItem
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.TextView
import androidx.recyclerview.widget.LinearLayoutManager
import androidx.recyclerview.widget.RecyclerView
import java.io.File
import kotlin.concurrent.thread
import uniffi.portputer_ffi.FileEntry
import uniffi.portputer_ffi.FileListing
import uniffi.portputer_ffi.PortputerException
import uniffi.portputer_ffi.ProgressListener

/**
 * The computer's files: folders to browse, files to download to the phone's
 * Downloads/Portputer, and Send Files, which uploads files from the phone
 * to the computer's Downloads folder.
 */
class FilesActivity : Activity(), Connection.Listener {
    private companion object {
        const val PICK = 1
    }

    private sealed interface Row {
        data class Parent(val path: String) : Row
        data class Item(val entry: FileEntry) : Row
    }

    private val main = Handler(Looper.getMainLooper())
    private val rows = mutableListOf<Row>()
    private val adapter = Adapter()
    private lateinit var status: TextView
    private lateinit var location: TextView
    private var listing: FileListing? = null
    private var busy = false
    private var lastAnnounced = -1

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_files)
        val name = Connection.info?.name ?: return finish()
        Ui.toolbar(this, getString(R.string.files_title, name), up = true)
        status = findViewById(R.id.status)
        location = findViewById(R.id.location)
        findViewById<RecyclerView>(R.id.list).apply {
            layoutManager = LinearLayoutManager(this@FilesActivity)
            adapter = this@FilesActivity.adapter
        }
        findViewById<Button>(R.id.send_files).setOnClickListener { pick() }
        findViewById<Button>(R.id.places).setOnClickListener { places() }
        open(savedInstanceState?.getString("path") ?: "")
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        listing?.let { outState.putString("path", it.path) }
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

    override fun onOptionsItemSelected(item: MenuItem): Boolean {
        if (item.itemId == android.R.id.home) {
            finish()
            return true
        }
        return super.onOptionsItemSelected(item)
    }

    private fun say(text: String) {
        status.text = text
    }

    /** Runs a transfer off the main thread, one at a time. */
    private fun work(start: String, task: () -> String) {
        if (busy) return say(getString(R.string.files_busy))
        busy = true
        lastAnnounced = -1
        say(start)
        thread(name = "portputer-files") {
            val result = try {
                task()
            } catch (e: PortputerException) {
                e.message ?: e.toString()
            } catch (e: java.io.IOException) {
                e.message ?: e.toString()
            }
            main.post {
                busy = false
                say(result)
            }
        }
    }

    /** Progress, said every 25 percent so it doesn't talk over everything. */
    private fun progress(label: String) = object : ProgressListener {
        override fun progress(done: ULong, total: ULong) {
            if (total == 0UL) return
            val percent = (done.toLong() * 100 / total.toLong()).toInt()
            val step = percent / 25 * 25
            if (step > lastAnnounced && step < 100) {
                lastAnnounced = step
                main.post { say(getString(R.string.files_progress, label, step)) }
            }
        }
    }

    private fun open(path: String) {
        val session = Connection.session() ?: return
        say(getString(R.string.files_loading))
        thread(name = "portputer-list") {
            val result = runCatching { session.listFiles(path) }
            main.post {
                result.onSuccess { show(it) }.onFailure { say(it.message ?: it.toString()) }
            }
        }
    }

    private fun show(l: FileListing) {
        listing = l
        location.text = l.path
        rows.clear()
        l.parent?.let { rows.add(Row.Parent(it)) }
        l.entries.forEach { rows.add(Row.Item(it)) }
        adapter.notifyDataSetChanged()
        val folder = File(l.path).name.ifEmpty { l.path }
        say(getString(R.string.files_listing, folder, l.entries.size))
    }

    private fun places() {
        val places = listing?.places ?: return
        AlertDialog.Builder(this)
            .setTitle(R.string.files_places)
            .setItems(places.map { it.name }.toTypedArray()) { _, i -> open(places[i].path) }
            .setNegativeButton(R.string.cancel, null)
            .show()
    }

    private fun download(entry: FileEntry) {
        val session = Connection.session() ?: return
        work(getString(R.string.files_downloading, entry.name)) {
            val dir = File(cacheDir, "download/${System.nanoTime()}").apply { mkdirs() }
            val saved = File(session.download(entry.path, dir.path, progress(entry.name)))
            val where = Files.saveToDownloads(this, saved)
            dir.deleteRecursively()
            getString(R.string.files_downloaded, saved.name, where)
        }
    }

    private fun pick() {
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE).setType("*/*")
            .putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
        startActivityForResult(intent, PICK)
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != PICK || resultCode != RESULT_OK || data == null) return
        val uris = data.clipData?.let { c -> (0 until c.itemCount).map { c.getItemAt(it).uri } } ?: listOfNotNull(data.data)
        val session = Connection.session() ?: return
        val name = Connection.info?.name ?: ""
        work(getString(R.string.files_sending, Files.count(this, uris.size))) {
            var sent = 0
            for (uri in uris) {
                val file = Files.copyToCache(this, uri) ?: continue
                try {
                    session.upload(file.path, file.name, null, progress(file.name))
                    sent++
                } finally {
                    file.parentFile?.deleteRecursively()
                }
            }
            getString(R.string.files_sent, Files.count(this, sent), name)
        }
    }

    private fun detail(e: FileEntry): String {
        val kind = if (e.dir) getString(R.string.files_folder) else Files.size(e.size.toLong())
        if (e.modified == 0UL) return kind
        val date = DateUtils.formatDateTime(this, e.modified.toLong() * 1000, DateUtils.FORMAT_SHOW_DATE or DateUtils.FORMAT_SHOW_TIME or DateUtils.FORMAT_ABBREV_ALL)
        return "$kind, $date"
    }

    private class Holder(view: View) : RecyclerView.ViewHolder(view) {
        val row: View = view.findViewById(R.id.row)
        val name: TextView = view.findViewById(R.id.name)
        val detail: TextView = view.findViewById(R.id.detail)
        val action: Button = view.findViewById(R.id.action)
    }

    private inner class Adapter : RecyclerView.Adapter<Holder>() {
        override fun getItemCount() = rows.size

        override fun onCreateViewHolder(parent: ViewGroup, viewType: Int) =
            Holder(LayoutInflater.from(parent.context).inflate(R.layout.item_file, parent, false))

        override fun onBindViewHolder(holder: Holder, position: Int) {
            when (val row = rows[position]) {
                is Row.Parent -> {
                    holder.name.text = getString(R.string.files_parent)
                    holder.detail.text = File(row.path).name.ifEmpty { row.path }
                    holder.row.contentDescription = getString(R.string.files_parent)
                    holder.row.setOnClickListener { open(row.path) }
                    Ui.clickLabel(holder.row, getString(R.string.files_open))
                    holder.action.visibility = View.GONE
                    Ui.setActions(holder.row, emptyList())
                }
                is Row.Item -> {
                    val e = row.entry
                    val detail = detail(e)
                    holder.name.text = e.name
                    holder.detail.text = detail
                    holder.row.contentDescription = "${e.name}, $detail"
                    if (e.dir) {
                        holder.row.setOnClickListener { open(e.path) }
                        Ui.clickLabel(holder.row, getString(R.string.files_open))
                        holder.action.visibility = View.GONE
                        Ui.setActions(holder.row, emptyList())
                    } else {
                        holder.row.setOnClickListener { download(e) }
                        Ui.clickLabel(holder.row, getString(R.string.files_download))
                        holder.action.visibility = View.VISIBLE
                        holder.action.setText(R.string.files_download)
                        holder.action.setOnClickListener { download(e) }
                        Ui.setActions(holder.row, emptyList())
                    }
                }
            }
        }
    }
}
