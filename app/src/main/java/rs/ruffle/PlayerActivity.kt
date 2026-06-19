package rs.ruffle

import android.annotation.SuppressLint
import android.app.AlarmManager
import android.app.AlertDialog
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.res.Configuration
import android.graphics.Color
import android.graphics.Typeface
import android.net.Uri
import android.os.Build
import android.os.Build.VERSION_CODES
import android.os.Bundle
import android.text.InputType
import android.view.KeyEvent
import android.util.Log
import android.util.TypedValue
import android.view.Menu
import android.view.MenuItem
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup
import android.view.Window
import android.view.WindowManager
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import android.widget.Button
import android.widget.PopupMenu
import android.widget.TextView
import androidx.constraintlayout.widget.ConstraintLayout
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.google.androidgamesdk.GameActivity
import java.io.DataInputStream
import java.io.File
import java.io.IOException
import kotlin.system.exitProcess

class PlayerActivity : GameActivity() {
    private enum class RenderBackend(val key: String, val label: String) {
        AUTO("auto", "自动"),
        VULKAN("vulkan", "Vulkan"),
        OPENGL("opengl", "OpenGL ES"),
    }

    @Suppress("unused")
    // Used by Rust
    private val swfBytes: ByteArray?
        get() {
            val uri = intent.data
            if (uri?.scheme == "content") {
                try {
                    contentResolver.openInputStream(uri).use { inputStream ->
                        if (inputStream == null) {
                            return null
                        }
                        val bytes = ByteArray(inputStream.available())
                        val dataInputStream = DataInputStream(inputStream)
                        dataInputStream.readFully(bytes)
                        return bytes
                    }
                } catch (ignored: IOException) {
                }
            }
            return null
        }

    @Suppress("unused")
    // Used by Rust
    private val swfUri: String?
        get() {
            return intent.dataString
        }

    @Suppress("unused")
    // Used by Rust
    private val traceOutput: String?
        get() {
            return intent.getStringExtra("traceOutput")
        }

    @Suppress("unused")
    // Used by Rust
    private fun navigateToUrl(url: String?) {
        startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
    }

    private var loc = IntArray(2)

    @Suppress("unused")
    // Handle of an EventLoopProxy over in rust-land
    private val eventLoopHandle: Long = 0

    @Suppress("unused")
    // Used by Rust
    private val locInWindow: IntArray
        get() {
            mSurfaceView.getLocationInWindow(loc)
            return loc
        }

    @Suppress("unused")
    // Used by Rust
    private val surfaceWidth: Int
        get() = mSurfaceView.width

    @Suppress("unused")
    // Used by Rust
    private val surfaceHeight: Int
        get() = mSurfaceView.height

    private external fun keydown(keyTag: String)
    private external fun keyup(keyTag: String)
    private external fun commitText(text: String)
    private external fun requestContextMenu()
    private external fun runContextMenuCallback(index: Int)
    private external fun clearContextMenu()

    private lateinit var ruffleInputView: RuffleInputView
    private lateinit var serverMetricsView: TextView
    private lateinit var renderBackendButton: TextView

    private fun dp(value: Int): Int =
        TypedValue.applyDimension(
            TypedValue.COMPLEX_UNIT_DIP,
            value.toFloat(),
            resources.displayMetrics
        ).toInt()

    private fun currentRenderBackend(): RenderBackend {
        val key = getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .getString(KEY_RENDER_BACKEND, RenderBackend.VULKAN.key)
        return RenderBackend.values().firstOrNull { it.key == key } ?: RenderBackend.VULKAN
    }

    @Suppress("unused")
    // Used by Rust
    private fun getRenderBackend(): String {
        return currentRenderBackend().key
    }

    @Suppress("unused")
    // Used by Rust
    private fun showContextMenu(items: Array<String>) {
        runOnUiThread {
            val popup = PopupMenu(this, findViewById(R.id.button_cm))
            val menu = popup.menu
            if (Build.VERSION.SDK_INT >= VERSION_CODES.P) {
                menu.setGroupDividerEnabled(true)
            }
            var group = 1
            for (i in items.indices) {
                val elements = items[i].split(" ".toRegex(), limit = 4).toTypedArray()
                val enabled = elements[0].toBoolean()
                val separatorBefore = elements[1].toBoolean()
                val checked = elements[2].toBoolean()
                val caption = elements[3]
                if (separatorBefore) group += 1
                val item = menu.add(group, i, Menu.NONE, caption)
                item.setEnabled(enabled)
                if (checked) {
                    item.setCheckable(true)
                    item.setChecked(true)
                }
            }
            val exitItemId: Int = items.size
            menu.add(group, exitItemId, Menu.NONE, "Exit")
            popup.setOnMenuItemClickListener { item: MenuItem ->
                if (item.itemId == exitItemId) {
                    finish()
                } else {
                    runContextMenuCallback(item.itemId)
                }
                true
            }
            popup.setOnDismissListener { clearContextMenu() }
            popup.show()
        }
    }

    @Suppress("unused")
    // Used by Rust
    private fun getAndroidDataStorageDir(): String {
        // TODO It can also be placed in an external storage path in the future to share archived content
        val storageDirPath = "${filesDir.absolutePath}/ruffle/shared_objects"
        val storageDir = File(storageDirPath)
        if (!storageDir.exists()) {
            storageDir.mkdirs()
        }
        return storageDirPath
    }

    @Suppress("unused")
    // Used by Rust
    private fun getAndroidAppDataDir(): String {
        return filesDir.absolutePath
    }

    private var loadFailureShown = false

    @Suppress("unused")
    // Used by Rust
    private fun showLoadFailureAndExit(message: String) {
        runOnUiThread {
            if (loadFailureShown || isFinishing) {
                return@runOnUiThread
            }
            loadFailureShown = true
            AlertDialog.Builder(this)
                .setTitle("\u6e38\u620f\u52a0\u8f7d\u5931\u8d25")
                .setMessage(message)
                .setPositiveButton("\u9000\u51fa") { _, _ -> finish() }
                .setOnCancelListener { finish() }
                .show()
        }
    }

    override fun onCreateSurfaceView() {
        val inflater = layoutInflater

        @SuppressLint("InflateParams")
        val layout = inflater.inflate(R.layout.keyboard, null) as ConstraintLayout

        contentViewId = View.generateViewId()
        layout.id = contentViewId
        setContentView(layout)
        mSurfaceView = InputEnabledSurfaceView(this)
        ruffleInputView = RuffleInputView(this)

        mSurfaceView.contentDescription = "Ruffle Player"
        mSurfaceView.isFocusable = true
        mSurfaceView.isFocusableInTouchMode = true

        val placeholder = findViewById<View>(R.id.placeholder)
        val pars = placeholder.layoutParams as ConstraintLayout.LayoutParams
        val parent = placeholder.parent as ViewGroup
        val index = parent.indexOfChild(placeholder)
        parent.removeView(placeholder)
        parent.addView(mSurfaceView, index)
        mSurfaceView.setLayoutParams(pars)
        layout.addView(
            ruffleInputView,
            ConstraintLayout.LayoutParams(1, 1).apply {
                startToStart = ConstraintLayout.LayoutParams.PARENT_ID
                bottomToBottom = ConstraintLayout.LayoutParams.PARENT_ID
            }
        )
        serverMetricsView = TextView(this).apply {
            text = "hit:0\nexpired:0\nfetch:0\ncached:0\nchecked:0"
            setTextColor(Color.WHITE)
            setBackgroundColor(0x66000000)
            typeface = Typeface.MONOSPACE
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 11f)
            includeFontPadding = false
            setPadding(dp(6), dp(4), dp(6), dp(4))
            isClickable = false
            isFocusable = false
            importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
        }
        layout.addView(
            serverMetricsView,
            ConstraintLayout.LayoutParams(
                ConstraintLayout.LayoutParams.WRAP_CONTENT,
                ConstraintLayout.LayoutParams.WRAP_CONTENT
            ).apply {
                startToStart = ConstraintLayout.LayoutParams.PARENT_ID
                bottomToBottom = ConstraintLayout.LayoutParams.PARENT_ID
                marginStart = dp(8)
                bottomMargin = dp(8)
            }
        )
        renderBackendButton = TextView(this).apply {
            text = renderBackendButtonText(currentRenderBackend())
            setTextColor(Color.WHITE)
            setBackgroundColor(0x99000000.toInt())
            typeface = Typeface.DEFAULT_BOLD
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 11f)
            includeFontPadding = false
            setPadding(dp(8), dp(5), dp(8), dp(5))
            isClickable = true
            isFocusable = false
            importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
            setOnClickListener { showRenderBackendMenu() }
        }
        layout.addView(
            renderBackendButton,
            ConstraintLayout.LayoutParams(
                ConstraintLayout.LayoutParams.WRAP_CONTENT,
                ConstraintLayout.LayoutParams.WRAP_CONTENT
            ).apply {
                startToStart = ConstraintLayout.LayoutParams.PARENT_ID
                topToTop = ConstraintLayout.LayoutParams.PARENT_ID
                marginStart = dp(8)
                topMargin = dp(8)
            }
        )
        val keys = gatherAllDescendantsOfType<Button>(
            layout.getViewById(R.id.keyboard),
            Button::class.java
        )
        for (b in keys) {
            b.setOnTouchListener { view: View, motionEvent: MotionEvent ->
                val tag = view.tag as String
                if (motionEvent.action == MotionEvent.ACTION_DOWN) keydown(tag)
                if (motionEvent.action == MotionEvent.ACTION_UP) keyup(tag)
                view.performClick()
                false
            }
        }
        layout.findViewById<View>(R.id.button_kb).setOnClickListener {
            val keyboard = layout.getViewById(R.id.keyboard)
            if (keyboard.visibility == View.VISIBLE) {
                keyboard.visibility = View.GONE
            } else {
                keyboard.visibility = View.VISIBLE
            }
        }
        layout.findViewById<View>(R.id.button_cm)
            .setOnClickListener { requestContextMenu() }
        updateOverlayVisibility(resources.configuration)
        layout.requestLayout()
        mSurfaceView.requestFocus()
        mSurfaceView.holder.addCallback(this)
        ViewCompat.setOnApplyWindowInsetsListener(mSurfaceView, this)
        ViewCompat.setOnApplyWindowInsetsListener(layout) { _, insets ->
            applyImeInsets(insets)
            insets
        }
        ViewCompat.requestApplyInsets(layout)
    }

    override fun onConfigurationChanged(newConfig: Configuration) {
        super.onConfigurationChanged(newConfig)
        updateOverlayVisibility(newConfig)
    }

    private fun updateOverlayVisibility(config: Configuration) {
        val keyboard = findViewById<View>(R.id.keyboard) ?: return
        val toolbar = findViewById<View>(R.id.toolbar) ?: return
        val isLandscape = config.orientation == Configuration.ORIENTATION_LANDSCAPE
        val visibility = if (isLandscape) View.GONE else View.VISIBLE
        keyboard.visibility = visibility
        toolbar.visibility = visibility
    }

    private fun renderBackendButtonText(backend: RenderBackend): String {
        return "GPU:${backend.label}"
    }

    private fun showRenderBackendMenu() {
        val current = currentRenderBackend()
        val popup = PopupMenu(this, renderBackendButton)
        RenderBackend.values().forEachIndexed { index, backend ->
            val item = popup.menu.add(Menu.NONE, index, index, backend.label)
            item.isCheckable = true
            item.isChecked = backend == current
        }
        popup.setOnMenuItemClickListener { item ->
            val selected = RenderBackend.values().getOrNull(item.itemId)
                ?: return@setOnMenuItemClickListener true
            if (selected != currentRenderBackend()) {
                confirmRenderBackendSwitch(selected)
            }
            true
        }
        popup.show()
    }

    private fun confirmRenderBackendSwitch(backend: RenderBackend) {
        AlertDialog.Builder(this)
            .setTitle("\u5207\u6362\u6e32\u67d3\u65b9\u5f0f")
            .setMessage(
                "\u5c06\u5207\u6362\u5230 ${backend.label} \u5e76\u91cd\u542f\u6e38\u620f\u3002" +
                    "\u5f53\u524d\u6e38\u620f\u753b\u9762\u4f1a\u91cd\u65b0\u52a0\u8f7d\u3002"
            )
            .setPositiveButton("\u5207\u6362\u5e76\u91cd\u542f") { _, _ ->
                getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
                    .edit()
                    .putString(KEY_RENDER_BACKEND, backend.key)
                    .commit()
                renderBackendButton.text = renderBackendButtonText(backend)
                restartApplication()
            }
            .setNegativeButton("\u53d6\u6d88", null)
            .show()
    }

    private fun restartApplication() {
        val launchIntent = packageManager.getLaunchIntentForPackage(packageName)
            ?: Intent(this, MainActivity::class.java)
        launchIntent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TASK)
        val flags = PendingIntent.FLAG_CANCEL_CURRENT or PendingIntent.FLAG_IMMUTABLE
        val pendingIntent = PendingIntent.getActivity(
            this,
            RESTART_REQUEST_CODE,
            launchIntent,
            flags
        )
        val alarmManager = getSystemService(Context.ALARM_SERVICE) as AlarmManager
        alarmManager.set(AlarmManager.RTC, System.currentTimeMillis() + 300L, pendingIntent)
        finishAndRemoveTask()
        android.os.Process.killProcess(android.os.Process.myPid())
        exitProcess(0)
    }

    @Suppress("unused")
    // Used by Rust
    private fun updateServerMetrics(text: String) {
        runOnUiThread {
            if (!::serverMetricsView.isInitialized) {
                return@runOnUiThread
            }
            serverMetricsView.text = text
        }
    }

    @Suppress("unused")
    // Used by Rust
    private fun showVirtualKeyboard() {
        runOnUiThread {
            if (!::ruffleInputView.isInitialized) {
                return@runOnUiThread
            }
            Log.i("ruffle", "Showing virtual keyboard")
            window.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE)
            ruffleInputView.isFocusable = true
            ruffleInputView.isFocusableInTouchMode = true
            ruffleInputView.requestFocus()
            WindowInsetsControllerCompat(window, ruffleInputView)
                .show(WindowInsetsCompat.Type.ime())
            val imm = getSystemService(Context.INPUT_METHOD_SERVICE) as InputMethodManager
            imm.showSoftInput(ruffleInputView, InputMethodManager.SHOW_IMPLICIT)
        }
    }

    @Suppress("unused")
    // Used by Rust
    private fun hideVirtualKeyboard() {
        runOnUiThread {
            if (!::ruffleInputView.isInitialized) {
                return@runOnUiThread
            }
            Log.i("ruffle", "Hiding virtual keyboard")
            val imm = getSystemService(Context.INPUT_METHOD_SERVICE) as InputMethodManager
            imm.hideSoftInputFromWindow(ruffleInputView.windowToken, 0)
            WindowInsetsControllerCompat(window, ruffleInputView)
                .hide(WindowInsetsCompat.Type.ime())
            ruffleInputView.isFocusable = false
            ruffleInputView.isFocusableInTouchMode = false
            mSurfaceView.requestFocus()
        }
    }

    private fun applyImeInsets(insets: WindowInsetsCompat) {
        val imeVisible = insets.isVisible(WindowInsetsCompat.Type.ime())
        val imeBottom = insets.getInsets(WindowInsetsCompat.Type.ime()).bottom
        val bottomMargin = if (imeVisible) imeBottom else 0
        val params = mSurfaceView.layoutParams as? ConstraintLayout.LayoutParams ?: return
        if (params.bottomMargin != bottomMargin) {
            params.bottomMargin = bottomMargin
            mSurfaceView.layoutParams = params
        }
        updateServerMetricsBottomMargin(bottomMargin)
    }

    private fun updateServerMetricsBottomMargin(bottomInset: Int) {
        if (!::serverMetricsView.isInitialized) {
            return
        }
        val params = serverMetricsView.layoutParams as? ConstraintLayout.LayoutParams ?: return
        val bottomMargin = dp(8) + bottomInset
        if (params.bottomMargin != bottomMargin) {
            params.bottomMargin = bottomMargin
            serverMetricsView.layoutParams = params
        }
    }

    private fun sendVirtualKey(tag: String) {
        keydown(tag)
        keyup(tag)
    }

    private fun commitImeText(text: CharSequence?) {
        val value = text?.toString() ?: return
        if (value.isEmpty()) {
            return
        }

        val typed = value.filter { it != '\n' && it != '\r' }
        if (typed.isNotEmpty()) {
            Log.i("ruffle", "Committing IME text: ${typed.length} chars")
            commitText(typed)
        }
    }

    private inner class RuffleInputView(context: Context) : View(context) {
        init {
            alpha = 0f
            isFocusable = false
            isFocusableInTouchMode = false
        }

        override fun onCheckIsTextEditor(): Boolean = true

        override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection {
            outAttrs.inputType =
                InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE
            outAttrs.imeOptions =
                EditorInfo.IME_ACTION_DONE or
                    EditorInfo.IME_FLAG_NO_EXTRACT_UI or
                    EditorInfo.IME_FLAG_NO_FULLSCREEN

            return object : BaseInputConnection(this, true) {
                override fun commitText(text: CharSequence?, newCursorPosition: Int): Boolean {
                    val handled = super.commitText(text, newCursorPosition)
                    commitImeText(text)
                    editable?.clear()
                    return handled
                }

                override fun deleteSurroundingText(
                    beforeLength: Int,
                    afterLength: Int
                ): Boolean {
                    sendVirtualKey("BACKSPACE")
                    editable?.clear()
                    return true
                }

                override fun deleteSurroundingTextInCodePoints(
                    beforeLength: Int,
                    afterLength: Int
                ): Boolean {
                    return deleteSurroundingText(beforeLength, afterLength)
                }

                override fun sendKeyEvent(event: KeyEvent): Boolean {
                    if (event.action == KeyEvent.ACTION_DOWN) {
                        when (event.keyCode) {
                            KeyEvent.KEYCODE_DEL -> {
                                sendVirtualKey("BACKSPACE")
                                editable?.clear()
                                return true
                            }
                            KeyEvent.KEYCODE_ENTER -> {
                                hideVirtualKeyboard()
                                editable?.clear()
                                return true
                            }
                        }
                    }
                    return super.sendKeyEvent(event)
                }

                override fun performEditorAction(actionCode: Int): Boolean {
                    hideVirtualKeyboard()
                    editable?.clear()
                    return true
                }
            }
        }
    }

    private fun hideSystemUI() {
        // This will put the game behind any cutouts and waterfalls on devices which have
        // them, so the corresponding insets will be non-zero.
        if (Build.VERSION.SDK_INT >= VERSION_CODES.R) {
            window.attributes.layoutInDisplayCutoutMode =
                WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS
        }
        // From API 30 onwards, this is the recommended way to hide the system UI, rather than
        // using View.setSystemUiVisibility.
        val decorView = window.decorView
        val controller = WindowInsetsControllerCompat(
            window,
            decorView
        )
        controller.hide(WindowInsetsCompat.Type.systemBars())
        controller.hide(WindowInsetsCompat.Type.displayCutout())
        controller.systemBarsBehavior =
            WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        nativeInit { message ->
            Log.e("ruffle", "Handling panic: $message")
            startActivity(
                Intent(this, PanicActivity::class.java).apply {
                    putExtra("message", message)
                }
            )
        }
        // When true, the app will fit inside any system UI windows.
        // When false, we render behind any system UI windows.
        WindowCompat.setDecorFitsSystemWindows(window, false)
        hideSystemUI()
        // You can set IME fields here or in native code using GameActivity_setImeEditorInfoFields.
        // We set the fields in native_engine.cpp.
        // super.setImeEditorInfoFields(InputType.TYPE_CLASS_TEXT,
        //     IME_ACTION_NONE, IME_FLAG_NO_FULLSCREEN );
        requestNoStatusBarFeature()
        supportActionBar?.hide()
        super.onCreate(savedInstanceState)
    }

    // Used by Rust
    @Suppress("unused")
    val isGooglePlayGames: Boolean
        get() {
            val pm = packageManager
            return pm.hasSystemFeature("com.google.android.play.feature.HPE_EXPERIENCE")
        }

    private fun requestNoStatusBarFeature() {
        // Hiding the status bar this way makes it see through when pulled down
        requestWindowFeature(Window.FEATURE_NO_TITLE)
        WindowInsetsControllerCompat(
            window,
            mSurfaceView
        ).hide(WindowInsetsCompat.Type.statusBars())
    }

    companion object {
        private const val PREFS_NAME = "ruffle_settings"
        private const val KEY_RENDER_BACKEND = "render_backend"
        private const val RESTART_REQUEST_CODE = 7337

        init {
            // load the native activity
            System.loadLibrary("ruffle_android")
        }

        @JvmStatic
        private external fun nativeInit(crashCallback: CrashCallback)

        private fun <T> gatherAllDescendantsOfType(v: View, t: Class<*>): List<T> {
            val result: MutableList<T> = ArrayList()
            @Suppress("UNCHECKED_CAST")
            if (t.isInstance(v)) result.add(v as T)
            if (v is ViewGroup) {
                for (i in 0 until v.childCount) {
                    result.addAll(gatherAllDescendantsOfType(v.getChildAt(i), t))
                }
            }
            return result
        }
    }

    fun interface CrashCallback {
        fun onCrash(message: String)
    }
}
