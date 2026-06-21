package rs.ruffle

import android.R
import android.content.ComponentName
import android.content.Intent
import android.net.Uri
import android.os.SystemClock
import androidx.test.espresso.matcher.ViewMatchers.assertThat
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.uiautomator.By
import androidx.test.uiautomator.UiDevice
import androidx.test.uiautomator.Until
import java.io.File
import java.util.concurrent.TimeoutException
import org.hamcrest.CoreMatchers.equalTo
import org.hamcrest.CoreMatchers.notNullValue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

private const val BASIC_SAMPLE_PACKAGE = "rs.seer2"
private const val LAUNCH_TIMEOUT = 60000L
private const val TRACE_TIMEOUT = 30000L

@RunWith(AndroidJUnit4::class)
class SmokeTest {
    private lateinit var device: UiDevice
    private lateinit var traceOutput: File
    private lateinit var swfFile: File

    @Before
    fun startMainActivityFromHomeScreen() {
        // Initialize UiDevice instance
        device = UiDevice.getInstance(InstrumentationRegistry.getInstrumentation())

        // Start from the home screen
        device.pressHome()

        // Wait for launcher
        val launcherPackage: String = device.launcherPackageName
        assertThat(launcherPackage, notNullValue())
        device.wait(
            Until.hasObject(By.pkg(launcherPackage).depth(0)),
            LAUNCH_TIMEOUT
        )

        // Launch the app
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        traceOutput = File.createTempFile("trace", ".txt", context.cacheDir)
        swfFile = File.createTempFile("movie", ".swf", context.cacheDir)
        val resources = InstrumentationRegistry.getInstrumentation().context.resources
        val inStream = resources.openRawResource(
            rs.ruffle.test.R.raw.helloflash
        )
        val bytes = inStream.readBytes()
        swfFile.writeBytes(bytes)
        startPlayerActivity(swfFile, traceOutput)

        // Wait for the app to appear
        device.wait(
            Until.hasObject(By.pkg(BASIC_SAMPLE_PACKAGE).depth(0)),
            LAUNCH_TIMEOUT
        )
        device.wait(
            Until.hasObject(By.desc("Ruffle Player")),
            LAUNCH_TIMEOUT
        )
    }

    @Test
    fun emulatorRunsASwf() {
        waitUntilTraceOutput()
        assertThat(device, notNullValue())

        val trace = traceOutput.readLines()
        assertThat(trace, equalTo(listOf("Hello from Flash!")))
    }

    private fun startPlayerActivity(swfFile: File, traceOutput: File) {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val intent = Intent(Intent.ACTION_VIEW).apply {
            component = ComponentName(BASIC_SAMPLE_PACKAGE, "rs.ruffle.PlayerActivity")
            data = Uri.fromFile(swfFile)
            putExtra("traceOutput", traceOutput.absolutePath)
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TASK)
        }
        context.startActivity(intent)
    }

    private fun waitUntilTraceOutput(timeoutMillis: Long = TRACE_TIMEOUT) {
        val timeoutAt = SystemClock.uptimeMillis() + timeoutMillis
        while (traceOutput.length() == 0L) {
            if (SystemClock.uptimeMillis() >= timeoutAt) {
                throw TimeoutException("No trace output was received within $timeoutMillis ms")
            }
            Thread.sleep(100)
        }
    }
}
