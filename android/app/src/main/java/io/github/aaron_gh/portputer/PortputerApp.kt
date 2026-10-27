package io.github.aaron_gh.portputer

import android.app.Application
import android.app.NotificationChannel
import android.app.NotificationManager

class PortputerApp : Application() {
    override fun onCreate() {
        super.onCreate()
        Connection.init(this)
        val channel = NotificationChannel(
            ConnectionService.CHANNEL, getString(R.string.notification_channel), NotificationManager.IMPORTANCE_LOW,
        )
        getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
    }
}
