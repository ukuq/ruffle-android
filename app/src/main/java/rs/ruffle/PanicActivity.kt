package rs.ruffle

import android.app.Activity
import android.os.Bundle
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView

class PanicActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        val message = intent.getStringExtra("message") ?: "Unknown"
        val container = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(32, 32, 32, 32)
        }
        container.addView(
            TextView(this).apply {
                text = "Ruffle Panicked"
                textSize = 24f
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
}
