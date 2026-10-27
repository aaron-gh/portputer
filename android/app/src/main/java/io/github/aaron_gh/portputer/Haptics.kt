package io.github.aaron_gh.portputer

import android.content.Context
import android.os.VibrationEffect
import android.os.Vibrator

object Haptics {
    fun tick(context: Context) {
        val vibrator = context.getSystemService(Vibrator::class.java) ?: return
        if (vibrator.hasVibrator()) vibrator.vibrate(VibrationEffect.createOneShot(30, VibrationEffect.DEFAULT_AMPLITUDE))
    }
}
