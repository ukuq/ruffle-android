package rs.ruffle

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.os.Handler
import android.os.Looper

class RestartActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        Handler(Looper.getMainLooper()).postDelayed({
            val swfUri = intent.getStringExtra(EXTRA_SWF_URI)?.takeIf { it.isNotBlank() }
            val targetIntent = if (swfUri == null) {
                Intent(this, MainActivity::class.java)
            } else {
                Intent(Intent.ACTION_VIEW, Uri.parse(swfUri), this, PlayerActivity::class.java)
            }
            targetIntent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TASK)
            startActivity(targetIntent)
            finish()
        }, RESTART_DELAY_MS)
    }

    companion object {
        const val EXTRA_SWF_URI = "rs.ruffle.extra.SWF_URI"
        private const val RESTART_DELAY_MS = 650L
    }
}
