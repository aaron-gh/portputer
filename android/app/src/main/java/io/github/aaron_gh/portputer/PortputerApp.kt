package io.github.aaron_gh.portputer

import android.app.Activity
import android.app.Application
import android.app.NotificationChannel
import android.app.NotificationManager
import android.os.Bundle
import android.view.WindowManager

class PortputerApp : Application() {
    private val started = mutableSetOf<Activity>()

    override fun onCreate() {
        super.onCreate()
        Connection.init(this)
        val channel = NotificationChannel(
            ConnectionService.CHANNEL, getString(R.string.notification_channel), NotificationManager.IMPORTANCE_LOW,
        )
        getSystemService(NotificationManager::class.java).createNotificationChannel(channel)

        // The screen stays on while connected to a computer.
        registerActivityLifecycleCallbacks(object : ActivityLifecycleCallbacks {
            override fun onActivityStarted(activity: Activity) {
                started.add(activity)
                keepScreenOn(activity)
            }

            override fun onActivityStopped(activity: Activity) {
                started.remove(activity)
            }

            override fun onActivityCreated(activity: Activity, savedInstanceState: Bundle?) {}
            override fun onActivityResumed(activity: Activity) {}
            override fun onActivityPaused(activity: Activity) {}
            override fun onActivitySaveInstanceState(activity: Activity, outState: Bundle) {}
            override fun onActivityDestroyed(activity: Activity) {}
        })
        Connection.addListener(object : Connection.Listener {
            override fun onConnectionChanged() = started.forEach(::keepScreenOn)
        })
    }

    private fun keepScreenOn(activity: Activity) {
        if (Connection.info != null) {
            activity.window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        } else {
            activity.window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        }
    }
}
