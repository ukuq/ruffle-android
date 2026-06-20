package rs.ruffle

import android.app.Activity
import android.content.Intent
import android.os.Bundle

class MainActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val pendingCrash = getSharedPreferences(CRASH_PREFS_NAME, MODE_PRIVATE)
            .getString(KEY_PENDING_CRASH, null)
        val target = if (pendingCrash.isNullOrEmpty()) {
            PlayerActivity::class.java
        } else {
            PanicActivity::class.java
        }
        startActivity(Intent(this, target))
        finish()
    }

    companion object {
        private const val CRASH_PREFS_NAME = "crash_logs"
        private const val KEY_PENDING_CRASH = "pending_native_panic"
    }
}
