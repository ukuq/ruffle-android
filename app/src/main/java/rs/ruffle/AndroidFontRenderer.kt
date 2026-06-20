package rs.ruffle

import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Rect
import android.graphics.Typeface
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.Locale
import kotlin.math.ceil
import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt

object AndroidFontRenderer {
    private const val SIZE_PX = 64f
    private const val SCALE = 20f
    private const val HEADER_BYTES = 20

    @JvmStatic
    fun metrics(family: String, bold: Boolean, italic: Boolean): IntArray {
        val fontMetrics = paint(family, bold, italic, Color.WHITE).fontMetrics
        return intArrayOf(
            (-fontMetrics.ascent * SCALE).roundToInt(),
            (fontMetrics.descent * SCALE).roundToInt(),
            (fontMetrics.leading * SCALE).roundToInt()
        )
    }

    @JvmStatic
    fun kerning(
        family: String,
        bold: Boolean,
        italic: Boolean,
        leftCodePoint: Int,
        rightCodePoint: Int
    ): Int {
        val paint = paint(family, bold, italic, Color.WHITE)
        val left = String(Character.toChars(leftCodePoint))
        val right = String(Character.toChars(rightCodePoint))
        val pair = left + right
        return (
            (paint.measureText(pair) - paint.measureText(left) - paint.measureText(right)) *
                SCALE
            )
            .roundToInt()
    }

    @JvmStatic
    fun renderGlyph(family: String, bold: Boolean, italic: Boolean, codePoint: Int): ByteArray? {
        if (!Character.isValidCodePoint(codePoint)) {
            return null
        }

        val text = String(Character.toChars(codePoint))
        val whitePaint = paint(family, bold, italic, Color.WHITE)
        if (!whitePaint.hasGlyph(text)) {
            return null
        }

        val fontMetrics = whitePaint.fontMetrics
        val ascent = ceil(-fontMetrics.ascent).toInt()
        val descent = ceil(fontMetrics.descent).toInt()
        val height = max(1, ascent + descent)
        val bounds = Rect()
        whitePaint.getTextBounds(text, 0, text.length, bounds)
        val advance = whitePaint.measureText(text)
        val left = min(0, bounds.left)
        val right = max(max(1, bounds.right), ceil(advance).toInt())
        val width = max(1, right - left)
        val baseline = ascent.toFloat()
        val x = -left.toFloat()

        val pixels = drawGlyph(width, height, x, baseline, text, whitePaint)
        val hasNativeColor = if (hasNonWhitePixels(pixels)) {
            val blackPixels = drawGlyph(
                width,
                height,
                x,
                baseline,
                text,
                paint(family, bold, italic, Color.BLACK)
            )
            hasNativeColor(pixels, blackPixels)
        } else {
            false
        }

        if (!hasNativeColor) {
            sharpenMonoGlyph(pixels)
        }

        val buffer = ByteBuffer
            .allocate(HEADER_BYTES + pixels.size)
            .order(ByteOrder.LITTLE_ENDIAN)
        buffer.putInt(width)
        buffer.putInt(height)
        buffer.putInt((advance * SCALE).roundToInt())
        buffer.putInt((-left * SCALE).roundToInt())
        buffer.putInt(if (hasNativeColor) 1 else 0)
        buffer.put(pixels)
        return buffer.array()
    }

    private fun paint(family: String, bold: Boolean, italic: Boolean, color: Int): Paint {
        val style = when {
            bold && italic -> Typeface.BOLD_ITALIC
            bold -> Typeface.BOLD
            italic -> Typeface.ITALIC
            else -> Typeface.NORMAL
        }
        return Paint(
            Paint.ANTI_ALIAS_FLAG or Paint.SUBPIXEL_TEXT_FLAG or Paint.FILTER_BITMAP_FLAG
        ).apply {
            textSize = SIZE_PX
            this.color = color
            typeface = Typeface.create(normalizeFamily(family), style)
        }
    }

    private fun normalizeFamily(family: String): String = when (family.lowercase(Locale.US)) {
        "android cjk", "arial", "simsun", "宋体" -> "sans-serif"
        "_sans" -> "sans-serif"
        "_serif" -> "serif"
        "_typewriter" -> "monospace"
        else -> family
    }

    private fun drawGlyph(
        width: Int,
        height: Int,
        x: Float,
        baseline: Float,
        text: String,
        paint: Paint
    ): ByteArray {
        val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
        bitmap.eraseColor(Color.TRANSPARENT)
        Canvas(bitmap).drawText(text, x, baseline, paint)

        val colors = IntArray(width * height)
        bitmap.getPixels(colors, 0, width, 0, 0, width, height)
        bitmap.recycle()

        val pixels = ByteArray(colors.size * 4)
        colors.forEachIndexed { index, color ->
            val offset = index * 4
            pixels[offset] = ((color ushr 16) and 0xFF).toByte()
            pixels[offset + 1] = ((color ushr 8) and 0xFF).toByte()
            pixels[offset + 2] = (color and 0xFF).toByte()
            pixels[offset + 3] = ((color ushr 24) and 0xFF).toByte()
        }
        return pixels
    }

    private fun hasNonWhitePixels(pixels: ByteArray): Boolean {
        var index = 0
        while (index < pixels.size) {
            val red = pixels[index].toInt() and 0xFF
            val green = pixels[index + 1].toInt() and 0xFF
            val blue = pixels[index + 2].toInt() and 0xFF
            val alpha = pixels[index + 3].toInt() and 0xFF
            if (alpha > 8 && (red < 240 || green < 240 || blue < 240)) {
                return true
            }
            index += 4
        }
        return false
    }

    private fun hasNativeColor(whitePixels: ByteArray, blackPixels: ByteArray): Boolean {
        var visiblePixels = 0
        var changedPixels = 0
        var index = 0
        while (index < whitePixels.size) {
            val whiteAlpha = whitePixels[index + 3].toInt() and 0xFF
            val blackAlpha = blackPixels[index + 3].toInt() and 0xFF
            if (max(whiteAlpha, blackAlpha) > 8) {
                visiblePixels += 1
                val delta =
                    kotlin.math.abs(
                        (whitePixels[index].toInt() and 0xFF) -
                            (blackPixels[index].toInt() and 0xFF)
                    ) +
                        kotlin.math.abs(
                            (whitePixels[index + 1].toInt() and 0xFF) -
                                (blackPixels[index + 1].toInt() and 0xFF)
                        ) +
                        kotlin.math.abs(
                            (whitePixels[index + 2].toInt() and 0xFF) -
                                (blackPixels[index + 2].toInt() and 0xFF)
                        )
                if (delta > 96) {
                    changedPixels += 1
                }
            }
            index += 4
        }
        return visiblePixels > 0 && changedPixels * 4 < visiblePixels
    }

    private fun sharpenMonoGlyph(pixels: ByteArray) {
        var index = 0
        while (index < pixels.size) {
            val alpha = pixels[index + 3].toInt() and 0xFF
            if (alpha != 0) {
                val boost = (alpha * (255 - alpha)) / (255 * 2)
                pixels[index] = 255.toByte()
                pixels[index + 1] = 255.toByte()
                pixels[index + 2] = 255.toByte()
                pixels[index + 3] = min(255, alpha + boost).toByte()
            }
            index += 4
        }
    }
}
