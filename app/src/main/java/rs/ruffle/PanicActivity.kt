package rs.ruffle

import android.app.Activity
import android.os.Bundle
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast
import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

class PanicActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        val crashPrefs = getSharedPreferences(CRASH_PREFS_NAME, MODE_PRIVATE)
        val message = intent.getStringExtra("message")
            ?: crashPrefs.getString(KEY_PENDING_CRASH, null)
            ?: "Unknown"
        val container = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(32, 32, 32, 32)
        }
        container.addView(
            TextView(this).apply {
                text = "游戏发生崩溃"
                textSize = 24f
            }
        )
        container.addView(
            Button(this).apply {
                text = "保存崩溃日志"
                setOnClickListener {
                    val path = writeCrashLog(message)
                    if (path != null) {
                        crashPrefs.edit().remove(KEY_PENDING_CRASH).apply()
                    }
                    Toast.makeText(
                        this@PanicActivity,
                        if (path != null) "已保存：$path" else "日志保存失败",
                        Toast.LENGTH_LONG
                    ).show()
                }
            }
        )
        container.addView(
            Button(this).apply {
                text = "不保存并退出"
                setOnClickListener {
                    crashPrefs.edit().remove(KEY_PENDING_CRASH).apply()
                    finishAndRemoveTask()
                }
            }
        )
        container.addView(
            ScrollView(this).apply {
                addView(
                    TextView(this@PanicActivity).apply {
                        text = message
                        setTextIsSelectable(true)
                        textSize = 12f
                    }
                )
            },
            LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT,
                0,
                1f
            )
        )

        setContentView(container)
    }

    private fun writeCrashLog(message: String): String? {
        return try {
            val root = getExternalFilesDir(null)?.parentFile ?: filesDir
            val dir = File(root, "errorlog")
            if (!dir.exists()) {
                dir.mkdirs()
            }
            val timestamp = SimpleDateFormat("yyyyMMdd-HHmmss-SSS", Locale.US).format(Date())
            val file = File(dir, "crash-$timestamp.log")
            file.writeText("Native panic\n\n$message", Charsets.UTF_8)
            file.absolutePath
        } catch (_: Exception) {
            null
        }
    }

    companion object {
        private const val CRASH_PREFS_NAME = "crash_logs"
        private const val KEY_PENDING_CRASH = "pending_native_panic"
    }
}
