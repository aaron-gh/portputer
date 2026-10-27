package io.github.aaron_gh.portputer

import android.app.Notification
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.os.IBinder
import android.os.PowerManager

/**
 * Keeps the connection and the audio going while the screen is off or
 * another app is in front, with a notification for switching the keyboard
 * and disconnecting. Holds a low-latency Wi-Fi lock for the audio.
 */
class ConnectionService : Service(), Connection.Listener {
    companion object {
        const val CHANNEL = "connection"
        private const val ID = 1
        private const val ACTION_TOGGLE = "toggle"
        private const val ACTION_DISCONNECT = "disconnect"

        fun start(context: Context) {
            context.startForegroundService(Intent(context, ConnectionService::class.java))
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, ConnectionService::class.java))
        }
    }

    private var wifi: WifiManager.WifiLock? = null
    /** For finding the computer again at a new address after it restarts. */
    private var multicast: WifiManager.MulticastLock? = null
    private var wake: PowerManager.WakeLock? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        val mode = if (Build.VERSION.SDK_INT >= 29) WifiManager.WIFI_MODE_FULL_LOW_LATENCY else {
            @Suppress("DEPRECATION")
            WifiManager.WIFI_MODE_FULL_HIGH_PERF
        }
        wifi = getSystemService(WifiManager::class.java)?.createWifiLock(mode, "portputer:audio")?.apply { acquire() }
        multicast = getSystemService(WifiManager::class.java)?.createMulticastLock("portputer:reconnect")?.apply {
            setReferenceCounted(false)
            acquire()
        }
        wake = getSystemService(PowerManager::class.java)?.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "portputer:connection")?.apply {
            setReferenceCounted(false)
            acquire()
        }
        Connection.addListener(this)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_TOGGLE -> Connection.toggleCapture(this)
            ACTION_DISCONNECT -> {
                Connection.disconnect()
                return START_NOT_STICKY
            }
        }
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(ID, notification(), ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK)
        } else {
            startForeground(ID, notification())
        }
        return START_NOT_STICKY
    }

    override fun onConnectionChanged() {
        if (Connection.state is Connection.State.Connected || Connection.state is Connection.State.Reconnecting) {
            getSystemService(NotificationManager::class.java).notify(ID, notification())
        }
    }

    private fun pending(action: String?, request: Int): PendingIntent {
        val intent = Intent(this, ConnectionService::class.java).setAction(action)
        return PendingIntent.getService(this, request, intent, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
    }

    private fun notification(): Notification {
        val info = Connection.info
        val name = info?.name ?: ""
        val title = when (Connection.state) {
            is Connection.State.Reconnecting -> getString(R.string.notification_reconnecting, name)
            else -> getString(R.string.notification_connected, name)
        }
        val text = getString(if (Connection.capturing) R.string.notification_capture_on else R.string.notification_capture_off)
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, ConnectedActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP), PendingIntent.FLAG_IMMUTABLE,
        )
        val toggleLabel = getString(if (Connection.capturing) R.string.notification_toggle_off else R.string.notification_toggle_on)
        return Notification.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(title)
            .setContentText(text)
            .setContentIntent(open)
            .setOngoing(true)
            .addAction(Notification.Action.Builder(null, toggleLabel, pending(ACTION_TOGGLE, 1)).build())
            .addAction(Notification.Action.Builder(null, getString(R.string.disconnect), pending(ACTION_DISCONNECT, 2)).build())
            .build()
    }

    override fun onDestroy() {
        Connection.removeListener(this)
        wifi?.takeIf { it.isHeld }?.release()
        multicast?.takeIf { it.isHeld }?.release()
        wake?.takeIf { it.isHeld }?.release()
        super.onDestroy()
    }
}
