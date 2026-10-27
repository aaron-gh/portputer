package io.github.aaron_gh.portputer

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.Settings as SystemSettings
import org.json.JSONObject
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.security.MessageDigest

/** A newer Portputer. */
data class Update(val version: String, val build: Long, val url: String, val sha256: String, val notes: String)

/**
 * Updates Portputer: reads the Android update feed, downloads a newer build,
 * verifies its SHA-256, and passes it to the package installer, which
 * requires confirmation and Portputer's signing key.
 */
object Updater {
    private const val FEED = "https://raw.githubusercontent.com/aaron-gh/portputer/main/appcast-android.json"
    private const val DAY_MS = 24L * 60 * 60 * 1000

    private val main = Handler(Looper.getMainLooper())

    private fun prefs(context: Context) = context.getSharedPreferences("updates", Context.MODE_PRIVATE)

    /** This app's build number, which the feed compares against. */
    fun installedBuild(context: Context): Long {
        val info = context.packageManager.getPackageInfo(context.packageName, 0)
        return if (Build.VERSION.SDK_INT >= 28) info.longVersionCode else {
            @Suppress("DEPRECATION")
            info.versionCode.toLong()
        }
    }

    fun installedVersion(context: Context): String =
        context.packageManager.getPackageInfo(context.packageName, 0).versionName ?: ""

    /** Whether a day has passed since the last automatic check. Notes this one. */
    fun dueForCheck(context: Context): Boolean {
        val last = prefs(context).getLong("last_check", 0)
        val now = System.currentTimeMillis()
        if (now - last < DAY_MS) return false
        prefs(context).edit().putLong("last_check", now).apply()
        return true
    }

    private fun open(url: String): HttpURLConnection =
        (URL(url).openConnection() as HttpURLConnection).apply {
            connectTimeout = 15_000
            readTimeout = 60_000
        }

    /** The newest build newer than this one, or null; [done] runs on the main thread. */
    fun check(context: Context, done: (Result<Update?>) -> Unit) {
        val app = context.applicationContext
        Thread {
            val result = try {
                val connection = open(FEED)
                try {
                    if (connection.responseCode != 200) throw Exception("The update feed answered ${connection.responseCode}.")
                    val json = JSONObject(connection.inputStream.bufferedReader().readText())
                    val update = Update(
                        json.getString("version"), json.getLong("build"), json.optString("url"),
                        json.optString("sha256"), json.optString("notes"),
                    )
                    Result.success(update.takeIf { it.url.isNotEmpty() && it.build > installedBuild(app) })
                } finally {
                    connection.disconnect()
                }
            } catch (e: Exception) {
                Result.failure(Exception(app.getString(R.string.update_unreachable)))
            }
            main.post { done(result) }
        }.start()
    }

    /** Whether Android lets this app install updates; if not, the settings page that allows it. */
    fun mayInstall(context: Context) = context.packageManager.canRequestPackageInstalls()

    fun allowInstallsIntent(context: Context) =
        Intent(SystemSettings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${context.packageName}"))

    /**
     * Downloads and installs [update], reporting progress to [said] on the
     * main thread. Android then asks for confirmation.
     */
    fun install(context: Context, update: Update, said: (String) -> Unit) {
        val app = context.applicationContext
        Thread {
            try {
                main.post { said(app.getString(R.string.update_downloading, update.version)) }
                val file = File(app.cacheDir, "update.apk")
                val connection = open(update.url)
                try {
                    if (connection.responseCode != 200) throw Exception(app.getString(R.string.update_download_failed, connection.responseCode))
                    file.outputStream().use { out -> connection.inputStream.use { it.copyTo(out) } }
                } finally {
                    connection.disconnect()
                }
                if (update.sha256.isNotEmpty()) {
                    val digest = MessageDigest.getInstance("SHA-256")
                    file.inputStream().use { input ->
                        val buffer = ByteArray(64 * 1024)
                        while (true) {
                            val n = input.read(buffer)
                            if (n < 0) break
                            digest.update(buffer, 0, n)
                        }
                    }
                    val got = digest.digest().joinToString("") { "%02x".format(it) }
                    if (!got.equals(update.sha256, ignoreCase = true)) {
                        file.delete()
                        throw Exception(app.getString(R.string.update_mismatch))
                    }
                }
                val installer = app.packageManager.packageInstaller
                val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL).apply {
                    setAppPackageName(app.packageName)
                    if (Build.VERSION.SDK_INT >= 31) setRequireUserAction(PackageInstaller.SessionParams.USER_ACTION_NOT_REQUIRED)
                }
                val id = installer.createSession(params)
                installer.openSession(id).use { session ->
                    session.openWrite("portputer.apk", 0, file.length()).use { out ->
                        file.inputStream().use { it.copyTo(out) }
                        session.fsync(out)
                    }
                    val status = PendingIntent.getBroadcast(
                        app, id, Intent(app, UpdateReceiver::class.java),
                        PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE,
                    )
                    session.commit(status.intentSender)
                }
                file.delete()
                main.post { said(app.getString(R.string.update_installing, update.version)) }
            } catch (e: Exception) {
                main.post { said(e.message ?: app.getString(R.string.update_failed)) }
            }
        }.start()
    }
}

/** Receives the package installer's status, and shows its confirmation when it needs one. */
class UpdateReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        when (intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)) {
            PackageInstaller.STATUS_PENDING_USER_ACTION -> {
                @Suppress("DEPRECATION")
                val confirm = if (Build.VERSION.SDK_INT >= 33) {
                    intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)
                } else {
                    intent.getParcelableExtra(Intent.EXTRA_INTENT)
                }
                confirm?.let { context.startActivity(it.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)) }
            }
            PackageInstaller.STATUS_SUCCESS -> {}
            else -> {
                val why = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE) ?: "unknown"
                android.widget.Toast.makeText(context, context.getString(R.string.update_install_failed, why), android.widget.Toast.LENGTH_LONG).show()
            }
        }
    }
}

/** The update flow shared by Settings (Check for Updates) and the daily check. */
object UpdatePrompt {
    /** Checks for a newer Portputer; [quietly] reports only an available update. */
    fun check(activity: android.app.Activity, quietly: Boolean, say: (String) -> Unit) {
        if (!quietly) say(activity.getString(R.string.update_checking))
        Updater.check(activity) { result ->
            if (activity.isFinishing) return@check
            result.onFailure { if (!quietly) say(it.message ?: "") }
            result.onSuccess { update ->
                if (update == null) {
                    if (!quietly) say(activity.getString(R.string.update_none, Updater.installedVersion(activity)))
                    return@onSuccess
                }
                val text = activity.getString(R.string.update_available, update.version, Updater.installedVersion(activity)) +
                    if (update.notes.isNotEmpty()) "\n\n${update.notes}" else ""
                android.app.AlertDialog.Builder(activity)
                    .setTitle(R.string.update_title)
                    .setMessage(text)
                    .setPositiveButton(R.string.update_install) { _, _ -> install(activity, update, say) }
                    .setNegativeButton(R.string.update_later, null)
                    .show()
            }
        }
    }

    private fun install(activity: android.app.Activity, update: Update, say: (String) -> Unit) {
        if (!Updater.mayInstall(activity)) {
            android.app.AlertDialog.Builder(activity)
                .setTitle(R.string.update_allow_title)
                .setMessage(R.string.update_allow_message)
                .setPositiveButton(R.string.ok) { _, _ -> activity.startActivity(Updater.allowInstallsIntent(activity)) }
                .setNegativeButton(R.string.cancel, null)
                .show()
            return
        }
        Updater.install(activity, update, say)
    }
}
