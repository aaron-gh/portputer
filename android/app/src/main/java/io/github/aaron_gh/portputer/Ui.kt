package io.github.aaron_gh.portputer

import android.app.Activity
import android.os.Build
import android.view.View
import android.view.WindowInsets
import android.widget.Toolbar
import androidx.core.view.ViewCompat

object Ui {
    /** Sets the screen's toolbar as its action bar, with Up when `up`. */
    fun toolbar(activity: Activity, title: CharSequence, up: Boolean) {
        val toolbar = activity.findViewById<Toolbar>(R.id.toolbar)
        activity.setActionBar(toolbar)
        activity.title = title
        activity.actionBar?.setDisplayHomeAsUpEnabled(up)
        fitBars(activity.findViewById(R.id.root))
    }

    /** Keeps the screen clear of the status and navigation bars, which apps draw under from Android 15. */
    fun fitBars(view: View) {
        val (left, top, right, bottom) = listOf(view.paddingLeft, view.paddingTop, view.paddingRight, view.paddingBottom)
        view.setOnApplyWindowInsetsListener { v, insets ->
            if (Build.VERSION.SDK_INT >= 30) {
                val bars = insets.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout() or WindowInsets.Type.ime())
                v.setPadding(left + bars.left, top + bars.top, right + bars.right, bottom + bars.bottom)
            } else {
                @Suppress("DEPRECATION")
                v.setPadding(
                    left + insets.systemWindowInsetLeft, top + insets.systemWindowInsetTop,
                    right + insets.systemWindowInsetRight, bottom + insets.systemWindowInsetBottom,
                )
            }
            insets
        }
    }

    /**
     * Replaces a view's custom accessibility actions. Screen readers offer
     * them in place of the buttons that are hidden from them.
     */
    fun setActions(view: View, actions: List<Pair<String, () -> Unit>>) {
        @Suppress("UNCHECKED_CAST")
        val old = view.getTag(R.id.accessibility_actions) as? List<Int> ?: emptyList()
        old.forEach { ViewCompat.removeAccessibilityAction(view, it) }
        val ids = actions.map { (label, run) ->
            ViewCompat.addAccessibilityAction(view, label) { _, _ -> run(); true }
        }
        view.setTag(R.id.accessibility_actions, ids)
    }

    /** Names what activating a view does ("Double tap to connect"). */
    fun clickLabel(view: View, label: String) {
        ViewCompat.replaceAccessibilityAction(
            view, androidx.core.view.accessibility.AccessibilityNodeInfoCompat.AccessibilityActionCompat.ACTION_CLICK, label, null,
        )
    }
}
