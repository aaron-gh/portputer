package io.github.aaron_gh.portputer

import android.content.Context
import android.content.Intent
import android.os.Handler
import android.os.Looper
import android.widget.Toast
import java.io.File
import java.util.concurrent.CopyOnWriteArraySet
import kotlin.concurrent.thread
import uniffi.portputer_ffi.ClientSession
import uniffi.portputer_ffi.FileInfo
import uniffi.portputer_ffi.PortputerException
import uniffi.portputer_ffi.Power
import uniffi.portputer_ffi.ServerInfo
import uniffi.portputer_ffi.SessionListener

/** Where to connect: an address, and the computer's ID when it's known. */
data class Target(val host: String, val port: Int, val id: String?, val name: String)

/**
 * The one connection to a computer, shared by the screens, the keyboard
 * service and the foreground service. Changes happen on the main thread.
 */
object Connection {
    sealed interface State {
        data object Idle : State
        data class Connecting(val target: Target) : State
        data class Connected(val info: ServerInfo) : State
        data class Reconnecting(val info: ServerInfo) : State
        data class Closed(val name: String, val reason: String) : State
    }

    interface Listener {
        fun onConnectionChanged() {}
        fun onMessage(text: String) {}
    }

    private val main = Handler(Looper.getMainLooper())
    private val listeners = CopyOnWriteArraySet<Listener>()
    private lateinit var app: Context
    lateinit var storeDir: String
        private set

    @Volatile var state: State = State.Idle
        private set
    @Volatile private var session: ClientSession? = null
    private var generation = 0

    /** Keys from the external keyboard go to the computer. */
    @Volatile var capturing = false
        private set
    var audioOn = false
        private set
    var audioWanted = false
        private set
    var micWanted = false
        private set
    var micOn = false
        private set
    /** Use Internet Relay: off for each connection. */
    var useRelay = false
        private set
    private var clipboard: ClipboardSync? = null

    /** The session, for transfers, which run on their own threads. */
    fun session(): ClientSession? = if (canSend()) session else null

    /** Sends the phone's clipboard if it changed, as Portputer comes to the front. */
    fun clipboardFocus() {
        clipboard?.send()
    }

    fun setShareClipboard(on: Boolean) {
        Settings(app).shareClipboard = on
        updateClipboard()
    }

    private fun updateClipboard() {
        val info = (state as? State.Connected)?.info
        val want = info != null && info.trusted && info.files && Settings(app).shareClipboard
        val s = session
        if (want && clipboard == null && s != null) {
            clipboard = ClipboardSync(app, s, info!!.name).also { it.start() }
        } else if (!want && clipboard != null) {
            clipboard?.stop()
            clipboard = null
        }
    }

    /** Modifiers held from the key menu. */
    val held = linkedSetOf<String>()

    fun init(context: Context) {
        app = context.applicationContext
        storeDir = File(app.filesDir, "client").apply { mkdirs() }.path
    }

    fun addListener(l: Listener) = listeners.add(l)
    fun removeListener(l: Listener) = listeners.remove(l)

    private fun changed() = listeners.forEach { it.onConnectionChanged() }
    private fun say(text: String) = listeners.forEach { it.onMessage(text) }

    val info: ServerInfo?
        get() = when (val s = state) {
            is State.Connected -> s.info
            is State.Reconnecting -> s.info
            else -> null
        }

    /** Connected to a paired computer: keys and commands go through. */
    fun canSend(): Boolean = (state as? State.Connected)?.info?.trusted == true

    fun connect(target: Target) {
        disconnect()
        val gen = ++generation
        state = State.Connecting(target)
        changed()
        val name = Settings(app).deviceName
        thread(name = "portputer-connect") {
            try {
                val s = ClientSession.connect(target.host, target.port.toUShort(), target.id, storeDir, name, false, Relay(gen))
                main.post {
                    if (gen != generation) {
                        s.disconnect()
                        s.close()
                        return@post
                    }
                    session = s
                    val settings = Settings(app)
                    s.setLatencyMs(settings.bufferMs.toUInt())
                    s.setVolume(settings.volume / 100f)
                    s.setSpeakersMuted(settings.muteSpeakers)
                    s.info()?.let { onConnected(it) }
                    ConnectionService.start(app)
                }
            } catch (e: PortputerException) {
                main.post {
                    if (gen == generation) {
                        state = State.Closed(target.name, app.getString(R.string.connect_failed, target.name, e.message ?: ""))
                        changed()
                    }
                }
            }
        }
    }

    private fun onConnected(info: ServerInfo) {
        val wasTrusted = this.info?.trusted == true
        state = State.Connected(info)
        updateClipboard()
        if (info.trusted && !wasTrusted) {
            val settings = Settings(app)
            if (settings.captureOnConnect && KeyboardService.isEnabled(app) && !capturing) {
                capturing = true
            }
            if (settings.audioOnConnect && info.audio && !audioWanted) {
                setAudio(true)
            }
        }
        changed()
    }

    fun disconnect() {
        generation++
        clipboard?.stop()
        clipboard = null
        session?.let {
            it.disconnect()
            it.close()
        }
        session = null
        capturing = false
        audioOn = false
        audioWanted = false
        micWanted = false
        micOn = false
        useRelay = false
        held.clear()
        if (state !is State.Idle) {
            state = State.Idle
            changed()
        }
        ConnectionService.stop(app)
    }

    fun pair(code: String, done: (String?) -> Unit) {
        val s = session ?: return done(app.getString(R.string.not_connected))
        thread(name = "portputer-pair") {
            val error = try {
                s.pair(code)
                null
            } catch (e: PortputerException) {
                e.message ?: e.toString()
            }
            main.post { done(error) }
        }
    }

    fun key(code: String, down: Boolean, repeat: Boolean) {
        if (canSend()) session?.key(code, down, repeat)
    }

    fun tap(code: String, modifiers: List<String>) {
        if (canSend()) session?.tap(code, modifiers)
    }

    fun text(text: String) {
        if (canSend()) session?.text(text)
    }

    fun hold(code: String, down: Boolean) {
        if (!canSend()) return
        session?.key(code, down, false)
        if (down) held.add(code) else held.remove(code)
        changed()
    }

    fun releaseAll() {
        session?.releaseAll()
        held.clear()
        changed()
    }

    fun power(action: Power, user: String? = null, password: String? = null) {
        if (canSend()) session?.power(action, user, password)
    }

    fun setCapturing(on: Boolean) {
        if (on == capturing) return
        if (!on) session?.releaseAll()
        capturing = on
        changed()
    }

    /** From the release shortcut: switches, and says so. */
    fun toggleCapture(context: Context) {
        setCapturing(!capturing)
        val name = info?.name ?: ""
        val text = if (capturing) context.getString(R.string.capture_toast_on, name) else context.getString(R.string.capture_toast_off)
        Toast.makeText(context, text, Toast.LENGTH_SHORT).show()
        if (Settings(context).haptics) Haptics.tick(context)
    }

    fun setAudio(on: Boolean) {
        audioWanted = on
        if (on) session?.startAudio() else session?.stopAudio()
        changed()
    }

    /** Sends the phone's microphone to the computer. Needs RECORD_AUDIO. */
    fun setMic(on: Boolean) {
        micWanted = on
        if (on) session?.startMic() else session?.stopMic()
        // The foreground service is started again with the microphone type.
        if (on) ConnectionService.start(app)
        changed()
    }

    /** Whether the connected computer can be reached through the relay. */
    val relayAvailable: Boolean
        get() = session?.relayAvailable() == true

    fun setUseRelay(on: Boolean) {
        useRelay = on
        session?.setUseRelay(on)
        changed()
    }

    fun setSpeakersMuted(muted: Boolean) {
        Settings(app).muteSpeakers = muted
        session?.setSpeakersMuted(muted)
        changed()
    }

    fun setVolume(percent: Int) {
        Settings(app).volume = percent
        session?.setVolume(percent / 100f)
    }

    fun setBuffer(ms: Int) {
        session?.setLatencyMs(ms.toUInt())
    }

    fun openConnectedScreen(context: Context) {
        context.startActivity(
            Intent(context, ConnectedActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP),
        )
    }

    /** Receives the core's events on its threads and applies them on the main thread. */
    private class Relay(private val gen: Int) : SessionListener {
        private fun post(block: () -> Unit) {
            main.post { if (gen == generation) block() }
        }

        override fun connected(info: ServerInfo) = post { onConnected(info) }

        override fun reconnecting() = post {
            val info = info ?: return@post
            state = State.Reconnecting(info)
            audioOn = false
            held.clear()
            changed()
        }

        override fun closed(reason: String) = post {
            val name = info?.name ?: (state as? State.Connecting)?.target?.name ?: ""
            // Stopped before the session is closed: a listener left on the
            // clipboard would call the closed session and crash.
            clipboard?.stop()
            clipboard = null
            session?.close()
            session = null
            capturing = false
            audioOn = false
            audioWanted = false
            micWanted = false
            micOn = false
            useRelay = false
            held.clear()
            state = State.Closed(name, app.getString(R.string.closed, name, reason))
            changed()
            ConnectionService.stop(app)
        }

        override fun notice(code: String, message: String) = post { say(message) }

        override fun error(code: String, message: String) = post {
            if (code == "audio_failed") {
                audioWanted = false
                changed()
            }
            if (code == "mic_failed") {
                micWanted = false
                changed()
            }
            if (code == "relay_failed") {
                useRelay = false
                changed()
            }
            say(message)
        }

        override fun audioStarted(sampleRate: UInt) = post {
            audioOn = true
            changed()
        }

        override fun audioStopped(reason: String) = post {
            audioOn = false
            changed()
        }

        override fun micStarted() = post {
            micOn = true
            changed()
        }

        override fun micStopped(reason: String) = post {
            micOn = false
            changed()
        }

        override fun clipboardText(text: String) = post { clipboard?.receivedText(text) }

        override fun clipboardFiles(files: List<FileInfo>) = post { clipboard?.receivedFiles(files) }

        override fun clipboardSynced(message: String) = post { say(message) }
    }
}
