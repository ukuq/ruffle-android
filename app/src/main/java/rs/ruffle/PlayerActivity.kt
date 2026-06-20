package rs.ruffle

import android.annotation.SuppressLint
import android.app.AlertDialog
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.graphics.Color
import android.graphics.Typeface
import android.media.AudioManager
import android.os.Build
import android.os.Build.VERSION_CODES
import android.os.Bundle
import android.os.Handler
import android.os.Looper
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
import android.widget.LinearLayout
import android.widget.PopupMenu
import android.widget.TextView
import androidx.constraintlayout.widget.ConstraintLayout
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.google.androidgamesdk.GameActivity
import java.io.File
import java.io.PrintWriter
import java.io.StringWriter
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import kotlin.system.exitProcess

class PlayerActivity : GameActivity() {
    private enum class RenderBackend(val key: String, val label: String) {
        AUTO("auto", "自动"),
        VULKAN("vulkan", "Vulkan"),
        OPENGL("opengl", "OpenGL ES"),
    }

    private enum class RenderScale(val key: String, val label: String, val value: Float) {
        NATIVE("1.0", "100%", 1.0f),
        BALANCED("0.75", "75%", 0.75f),
        PERFORMANCE("0.5", "50%", 0.5f),
    }

    @Suppress("unused")
    // Used by Rust
    private val traceOutput: String?
        get() {
            return intent.getStringExtra("traceOutput")
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
    private external fun reloadGame()

    private lateinit var ruffleInputView: RuffleInputView
    private lateinit var diagnosticOverlay: LinearLayout
    private lateinit var fpsView: TextView
    private lateinit var serverMetricsView: TextView
    private lateinit var versionView: TextView
    private lateinit var renderBackendButton: TextView
    private lateinit var renderScaleButton: TextView
    private val audioManager: AudioManager by lazy {
        getSystemService(Context.AUDIO_SERVICE) as AudioManager
    }

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

    private fun currentRenderScale(): RenderScale {
        val key = getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .getString(KEY_RENDER_SCALE, RenderScale.NATIVE.key)
        return RenderScale.values().firstOrNull { it.key == key } ?: RenderScale.NATIVE
    }

    @Suppress("unused")
    // Used by Rust
    private fun getRenderBackend(): String {
        return currentRenderBackend().key
    }

    @Suppress("unused")
    // Used by Rust
    private fun getRenderScale(): Float {
        return currentRenderScale().value
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
                    confirmExit()
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
        val appDataRoot = getExternalFilesDir(null)?.parentFile ?: filesDir
        if (!appDataRoot.exists()) {
            appDataRoot.mkdirs()
        }
        return appDataRoot.absolutePath
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
                .setPositiveButton("\u9000\u51fa") { _, _ -> exitApplication() }
                .setOnCancelListener { exitApplication() }
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
        diagnosticOverlay = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            isClickable = false
            isFocusable = false
            importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
        }
        fpsView = overlayTextView("FPS:0")
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
        versionView = overlayTextView("版本:${appVersionName()}")
        diagnosticOverlay.addView(fpsView)
        diagnosticOverlay.addView(serverMetricsView)
        diagnosticOverlay.addView(versionView)
        layout.addView(
            diagnosticOverlay,
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
            id = View.generateViewId()
            text = renderBackendButtonText(currentRenderBackend())
            setTextColor(Color.WHITE)
            setBackgroundColor(Color.TRANSPARENT)
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
        renderScaleButton = TextView(this).apply {
            text = renderScaleButtonText(currentRenderScale())
            setTextColor(Color.WHITE)
            setBackgroundColor(Color.TRANSPARENT)
            typeface = Typeface.DEFAULT_BOLD
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 11f)
            includeFontPadding = false
            setPadding(dp(8), dp(5), dp(8), dp(5))
            isClickable = true
            isFocusable = false
            importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
            setOnClickListener { showRenderScaleMenu() }
        }
        layout.addView(
            renderScaleButton,
            ConstraintLayout.LayoutParams(
                ConstraintLayout.LayoutParams.WRAP_CONTENT,
                ConstraintLayout.LayoutParams.WRAP_CONTENT
            ).apply {
                startToStart = ConstraintLayout.LayoutParams.PARENT_ID
                topToBottom = renderBackendButton.id
                marginStart = dp(8)
                topMargin = dp(6)
            }
        )
        addTopActionButtons(layout)
        addHealthNotice(layout)
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

    private fun renderScaleButtonText(scale: RenderScale): String {
        return "分辨率:${scale.label}"
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

    private fun showRenderScaleMenu() {
        val current = currentRenderScale()
        val popup = PopupMenu(this, renderScaleButton)
        RenderScale.values().forEachIndexed { index, scale ->
            val item = popup.menu.add(Menu.NONE, index, index, scale.label)
            item.isCheckable = true
            item.isChecked = scale == current
        }
        popup.setOnMenuItemClickListener { item ->
            val selected = RenderScale.values().getOrNull(item.itemId)
                ?: return@setOnMenuItemClickListener true
            if (selected != currentRenderScale()) {
                confirmRenderScaleSwitch(selected)
            }
            true
        }
        popup.show()
    }

    private fun confirmRenderScaleSwitch(scale: RenderScale) {
        AlertDialog.Builder(this)
            .setTitle("切换分辨率")
            .setMessage("将切换到 ${scale.label} 并重启游戏。当前游戏画面会重新加载。")
            .setPositiveButton("切换并重启") { _, _ ->
                getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
                    .edit()
                    .putString(KEY_RENDER_SCALE, scale.key)
                    .commit()
                renderScaleButton.text = renderScaleButtonText(scale)
                restartApplication()
            }
            .setNegativeButton("取消", null)
            .show()
    }

    private fun confirmExit() {
        AlertDialog.Builder(this)
            .setTitle("退出游戏")
            .setMessage("确定要退出当前游戏吗？")
            .setPositiveButton("退出") { _, _ -> exitApplication() }
            .setNegativeButton("取消", null)
            .show()
    }

    private fun confirmRefresh() {
        AlertDialog.Builder(this)
            .setTitle("刷新游戏")
            .setMessage("确定要重新加载当前 Flash 吗？")
            .setPositiveButton("刷新") { _, _ -> reloadGame() }
            .setNegativeButton("取消", null)
            .show()
    }

    private fun exitApplication() {
        KeepAliveService.stop(this)
        finishAndRemoveTask()
        android.os.Process.killProcess(android.os.Process.myPid())
        exitProcess(0)
    }

    private fun restartApplication() {
        KeepAliveService.stop(this)
        startActivity(
            Intent(this, RestartActivity::class.java).apply {
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
        )
        finishAndRemoveTask()
        Handler(Looper.getMainLooper()).postDelayed({
            android.os.Process.killProcess(android.os.Process.myPid())
            exitProcess(0)
        }, 120L)
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
    private fun updateFps(text: String) {
        runOnUiThread {
            if (!::fpsView.isInitialized) {
                return@runOnUiThread
            }
            fpsView.text = text
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
        if (!::diagnosticOverlay.isInitialized) {
            return
        }
        val params = diagnosticOverlay.layoutParams as? ConstraintLayout.LayoutParams ?: return
        val bottomMargin = dp(8) + bottomInset
        if (params.bottomMargin != bottomMargin) {
            params.bottomMargin = bottomMargin
            diagnosticOverlay.layoutParams = params
        }
    }

    private fun overlayTextView(initialText: String): TextView {
        return TextView(this).apply {
            text = initialText
            setTextColor(Color.WHITE)
            setBackgroundColor(0x66000000)
            typeface = Typeface.MONOSPACE
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 11f)
            includeFontPadding = false
            setPadding(dp(6), dp(3), dp(6), dp(3))
            isClickable = false
            isFocusable = false
            importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
        }
    }

    @Suppress("DEPRECATION")
    private fun appVersionName(): String {
        val packageInfo = if (Build.VERSION.SDK_INT >= VERSION_CODES.TIRAMISU) {
            packageManager.getPackageInfo(packageName, PackageManager.PackageInfoFlags.of(0))
        } else {
            packageManager.getPackageInfo(packageName, 0)
        }
        return packageInfo.versionName ?: "unknown"
    }

    private fun actionButton(label: String, onClick: () -> Unit): TextView {
        return TextView(this).apply {
            text = label
            setTextColor(Color.WHITE)
            setBackgroundColor(Color.TRANSPARENT)
            typeface = Typeface.DEFAULT_BOLD
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 12f)
            includeFontPadding = false
            setPadding(dp(10), dp(6), dp(10), dp(6))
            isClickable = true
            isFocusable = false
            setOnClickListener { onClick() }
        }
    }

    private fun addTopActionButtons(layout: ConstraintLayout) {
        val container = LinearLayout(this).apply {
            id = View.generateViewId()
            orientation = LinearLayout.VERTICAL
            importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
        }
        container.addView(actionButton("刷新") { confirmRefresh() })
        container.addView(
            actionButton("退出") { confirmExit() },
            LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.WRAP_CONTENT,
                LinearLayout.LayoutParams.WRAP_CONTENT
            ).apply {
                topMargin = dp(8)
            }
        )
        layout.addView(
            container,
            ConstraintLayout.LayoutParams(
                ConstraintLayout.LayoutParams.WRAP_CONTENT,
                ConstraintLayout.LayoutParams.WRAP_CONTENT
            ).apply {
                endToEnd = ConstraintLayout.LayoutParams.PARENT_ID
                topToTop = ConstraintLayout.LayoutParams.PARENT_ID
                marginEnd = dp(8)
                topMargin = dp(8)
            }
        )
    }

    private fun addHealthNotice(layout: ConstraintLayout) {
        val notice = LinearLayout(this).apply {
            id = View.generateViewId()
            orientation = LinearLayout.VERTICAL
            gravity = android.view.Gravity.CENTER
            setBackgroundColor(Color.BLACK)
            isClickable = true
            importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
        }
        val declaration = TextView(this).apply {
            text = "抵制不良游戏，拒绝盗版游戏。\n注意自我保护，谨防受骗上当。\n适度游戏益脑，沉迷游戏伤身。\n合理安排时间，享受健康生活。"
            setTextColor(Color.WHITE)
            gravity = android.view.Gravity.CENTER
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 18f)
            includeFontPadding = false
        }
        val title = TextView(this).apply {
            text = "阿卡迪亚：传说 （by改服项目组）"
            setTextColor(Color.WHITE)
            gravity = android.view.Gravity.CENTER
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 16f)
            includeFontPadding = false
            setPadding(0, dp(18), 0, 0)
        }
        notice.addView(declaration)
        notice.addView(title)
        layout.addView(
            notice,
            ConstraintLayout.LayoutParams(
                ConstraintLayout.LayoutParams.MATCH_PARENT,
                ConstraintLayout.LayoutParams.MATCH_PARENT
            ).apply {
                startToStart = ConstraintLayout.LayoutParams.PARENT_ID
                endToEnd = ConstraintLayout.LayoutParams.PARENT_ID
                topToTop = ConstraintLayout.LayoutParams.PARENT_ID
                bottomToBottom = ConstraintLayout.LayoutParams.PARENT_ID
            }
        )
        notice.bringToFront()
        Handler(Looper.getMainLooper()).postDelayed({
            notice.visibility = View.GONE
        }, HEALTH_NOTICE_MS)
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
            getSharedPreferences(CRASH_PREFS_NAME, Context.MODE_PRIVATE)
                .edit()
                .putString(KEY_PENDING_CRASH, message)
                .commit()
            startActivity(
                Intent(this, PanicActivity::class.java).apply {
                    putExtra("message", message)
                }
            )
        }
        installCrashLogger()
        volumeControlStream = AudioManager.STREAM_MUSIC
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
        KeepAliveService.start(this)
    }

    override fun onDestroy() {
        if (isFinishing) {
            KeepAliveService.stop(this)
        }
        super.onDestroy()
    }

    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        val direction = when (event.keyCode) {
            KeyEvent.KEYCODE_VOLUME_UP -> AudioManager.ADJUST_RAISE
            KeyEvent.KEYCODE_VOLUME_DOWN -> AudioManager.ADJUST_LOWER
            else -> null
        }
        if (direction != null) {
            if (event.action == KeyEvent.ACTION_DOWN) {
                audioManager.adjustStreamVolume(
                    AudioManager.STREAM_MUSIC,
                    direction,
                    AudioManager.FLAG_SHOW_UI
                )
            }
            return true
        }
        return super.dispatchKeyEvent(event)
    }

    private fun installCrashLogger() {
        if (crashLoggerInstalled) {
            return
        }
        crashLoggerInstalled = true
        val previousHandler = Thread.getDefaultUncaughtExceptionHandler()
        Thread.setDefaultUncaughtExceptionHandler { thread, throwable ->
            writeCrashLog(
                "Java uncaught exception\n" +
                    "Thread: ${thread.name}\n\n" +
                    throwableStackTrace(throwable)
            )
            if (previousHandler != null) {
                previousHandler.uncaughtException(thread, throwable)
            } else {
                exitProcess(2)
            }
        }
    }

    private fun throwableStackTrace(throwable: Throwable): String {
        val writer = StringWriter()
        throwable.printStackTrace(PrintWriter(writer))
        return writer.toString()
    }

    private fun writeCrashLog(message: String) {
        try {
            val dir = File(getAndroidAppDataDir(), "errorlog")
            if (!dir.exists()) {
                dir.mkdirs()
            }
            val timestamp = SimpleDateFormat("yyyyMMdd-HHmmss-SSS", Locale.US).format(Date())
            File(dir, "crash-$timestamp.log").writeText(message, Charsets.UTF_8)
        } catch (error: Exception) {
            Log.e("ruffle", "Failed to write crash log", error)
        }
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
        private const val KEY_RENDER_SCALE = "render_scale"
        private const val CRASH_PREFS_NAME = "crash_logs"
        private const val KEY_PENDING_CRASH = "pending_native_panic"
        private const val HEALTH_NOTICE_MS = 1000L
        private var crashLoggerInstalled = false

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
