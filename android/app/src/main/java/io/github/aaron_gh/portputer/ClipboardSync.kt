package io.github.aaron_gh.portputer

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.provider.OpenableColumns
import android.widget.Toast
import androidx.core.content.FileProvider
import java.io.File
import kotlin.concurrent.thread
import uniffi.portputer_ffi.ClientSession
import uniffi.portputer_ffi.FileInfo
import uniffi.portputer_ffi.PortputerException
import uniffi.portputer_ffi.ProgressListener

/**
 * Keeps the phone's clipboard and the computer's in step: text both ways,
 * and files, which are transferred and put on the other side's clipboard.
 * Android only lets the app in front read the clipboard, so the phone's
 * clipboard is sent when it changes while Portputer is open, and when
 * Portputer comes to the front.
 */
class ClipboardSync(private val context: Context, private val session: ClientSession, private val computer: String) {
    companion object {
        /** Files copied on the computer up to this size are fetched by themselves. */
        const val AUTO_FETCH_LIMIT = 100L * 1024 * 1024
        const val AUTHORITY = "io.github.aaron_gh.portputer.files"
    }

    private val main = Handler(Looper.getMainLooper())
    private val clipboard = context.getSystemService(ClipboardManager::class.java)
    /** What Portputer last put on, or sent from, the phone's clipboard, so it isn't sent back. */
    private var last: String? = null

    private val listener = ClipboardManager.OnPrimaryClipChangedListener { send() }

    fun start() {
        clipboard.addPrimaryClipChangedListener(listener)
        session.setClipboardWatch(true)
    }

    fun stop() {
        clipboard.removePrimaryClipChangedListener(listener)
        session.setClipboardWatch(false)
    }

    private fun key(clip: ClipData): String = (0 until clip.itemCount).joinToString("\n") { i ->
        val item = clip.getItemAt(i)
        item.uri?.toString() ?: item.text?.toString() ?: ""
    }

    /** Sends the phone's clipboard if it's new. Reading fails unless Portputer is in front. */
    fun send() {
        val clip = runCatching { clipboard.primaryClip }.getOrNull() ?: return
        if (clip.itemCount == 0) return
        val key = key(clip)
        if (key == last || key.isEmpty()) return
        last = key
        val uris = (0 until clip.itemCount).mapNotNull { clip.getItemAt(it).uri }
        if (uris.isEmpty()) {
            val text = clip.getItemAt(0).coerceToText(context)?.toString() ?: return
            session.sendClipboardText(text)
            return
        }
        val batch = System.currentTimeMillis().toULong()
        thread(name = "portputer-clipboard-send") {
            var sent = 0
            for (uri in uris) {
                val file = Files.copyToCache(context, uri) ?: continue
                try {
                    session.upload(file.path, file.name, batch, NoProgress)
                    sent++
                } catch (e: PortputerException) {
                    toast(e.message ?: "")
                } finally {
                    file.delete()
                }
            }
            if (sent > 0) toast(context.getString(R.string.clipboard_files_sent, Files.count(context, sent), computer))
        }
    }

    fun receivedText(text: String) {
        last = text
        clipboard.setPrimaryClip(ClipData.newPlainText(computer, text))
    }

    fun receivedFiles(files: List<FileInfo>) {
        val total = files.sumOf { it.size.toLong() }
        if (total > AUTO_FETCH_LIMIT) {
            toast(context.getString(R.string.clipboard_files_too_big, Files.count(context, files.size), Files.size(total), computer))
            return
        }
        thread(name = "portputer-clipboard-fetch") {
            val dir = File(context.cacheDir, "clipboard/${System.currentTimeMillis()}").apply { mkdirs() }
            try {
                val paths = session.clipboardFiles(dir.path, NoProgress)
                if (paths.isEmpty()) return@thread
                val uris = paths.map { FileProvider.getUriForFile(context, AUTHORITY, File(it)) }
                val clip = ClipData.newUri(context.contentResolver, File(paths[0]).name, uris[0])
                uris.drop(1).forEach { clip.addItem(ClipData.Item(it)) }
                main.post {
                    last = key(clip)
                    clipboard.setPrimaryClip(clip)
                }
                toast(context.getString(R.string.clipboard_files_received, Files.count(context, paths.size), computer))
            } catch (e: PortputerException) {
                toast(e.message ?: "")
            }
        }
    }

    private fun toast(text: String) = main.post { Toast.makeText(context, text, Toast.LENGTH_SHORT).show() }

    private object NoProgress : ProgressListener {
        override fun progress(done: ULong, total: ULong) {}
    }
}

/** Moving files between Android's storage and the file paths the core works with. */
object Files {
    fun displayName(context: Context, uri: Uri): String {
        context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
            if (c.moveToFirst()) c.getString(0)?.let { return it }
        }
        return uri.lastPathSegment?.substringAfterLast('/') ?: "file"
    }

    /** Copies a content URI to the cache, keeping its name. */
    fun copyToCache(context: Context, uri: Uri): File? {
        val name = displayName(context, uri)
        val dir = File(context.cacheDir, "upload/${System.nanoTime()}").apply { mkdirs() }
        val file = File(dir, name.replace('/', '_'))
        return runCatching {
            context.contentResolver.openInputStream(uri)!!.use { input -> file.outputStream().use { input.copyTo(it) } }
            file
        }.getOrNull()
    }

    /** Moves a downloaded file into Downloads/Portputer, returning the folder it's in, as shown. */
    fun saveToDownloads(context: Context, file: File): String {
        if (android.os.Build.VERSION.SDK_INT >= 29) {
            val values = android.content.ContentValues().apply {
                put(android.provider.MediaStore.Downloads.DISPLAY_NAME, file.name)
                put(android.provider.MediaStore.Downloads.RELATIVE_PATH, "Download/Portputer")
                put(android.provider.MediaStore.Downloads.IS_PENDING, 1)
            }
            val resolver = context.contentResolver
            val uri = resolver.insert(android.provider.MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
                ?: throw java.io.IOException("Downloads refused the file")
            resolver.openOutputStream(uri)!!.use { out -> file.inputStream().use { it.copyTo(out) } }
            values.clear()
            values.put(android.provider.MediaStore.Downloads.IS_PENDING, 0)
            resolver.update(uri, values, null, null)
            file.delete()
            return "Download/Portputer"
        }
        val dir = File(context.getExternalFilesDir(android.os.Environment.DIRECTORY_DOWNLOADS), "Portputer").apply { mkdirs() }
        file.copyTo(File(dir, file.name), overwrite = true)
        file.delete()
        return dir.path
    }

    fun count(context: Context, n: Int): String = context.resources.getQuantityString(R.plurals.files, n, n)

    fun size(bytes: Long): String = when {
        bytes >= 1L shl 30 -> String.format("%.1f GB", bytes / (1L shl 30).toDouble())
        bytes >= 1L shl 20 -> String.format("%.1f MB", bytes / (1L shl 20).toDouble())
        bytes >= 1L shl 10 -> String.format("%.0f KB", bytes / (1L shl 10).toDouble())
        else -> "$bytes bytes"
    }
}
