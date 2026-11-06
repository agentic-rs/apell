package dev.apell.sensors

import android.app.Activity
import android.app.AlertDialog
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Color
import android.view.ViewGroup
import android.view.WindowManager
import android.widget.FrameLayout
import android.widget.ImageView
import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.DecodeHintType
import com.google.zxing.EncodeHintType
import com.google.zxing.RGBLuminanceSource
import com.google.zxing.common.HybridBinarizer
import com.google.zxing.qrcode.QRCodeReader
import com.google.zxing.qrcode.QRCodeWriter
import com.google.zxing.qrcode.decoder.ErrorCorrectionLevel
import java.io.File
import kotlin.math.roundToInt

internal object TransferQr {
  private const val PREFIX = "apell-settings:v1:"
  private const val MAX_PAYLOAD = 1200
  private const val MAX_PHOTO_BYTES = 32L * 1024 * 1024
  private const val MAX_PHOTO_EDGE = 2048

  fun isValid(payload: String): Boolean = payload.length in (PREFIX.length + 1)..MAX_PAYLOAD &&
    payload.startsWith(PREFIX) && payload.all { it in '!'..'~' }

  fun dialog(activity: Activity, payload: String, onDismiss: () -> Unit): AlertDialog {
    val density = activity.resources.displayMetrics.density
    val screen_width = activity.resources.displayMetrics.widthPixels
    val side = minOf(screen_width - (72 * density).roundToInt(), (440 * density).roundToInt())
    check(side > 0)
    val bitmap = bitmap(payload, side)
    val image = ImageView(activity).apply {
      contentDescription = "Encrypted settings QR code"
      scaleType = ImageView.ScaleType.FIT_CENTER
      setImageBitmap(bitmap)
      setBackgroundColor(Color.WHITE)
      layoutParams = FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, side)
    }
    val container = FrameLayout(activity).apply { addView(image) }
    val dialog = AlertDialog.Builder(activity)
      .setTitle("Transfer settings")
      .setMessage("Scan this encrypted QR code in Apell. Share the passphrase separately.")
      .setView(container)
      .setPositiveButton("Done", null)
      .create()
    // Secure the dedicated window before the QR view is ever attached to it.
    // Closing the dialog removes the secure window without changing Apell's
    // main window or any other feature's capture settings.
    checkNotNull(dialog.window).addFlags(WindowManager.LayoutParams.FLAG_SECURE)
    dialog.setOnDismissListener {
      image.setImageDrawable(null)
      onDismiss()
    }
    return dialog
  }

  private fun bitmap(payload: String, side: Int): Bitmap {
    val matrix = QRCodeWriter().encode(payload, BarcodeFormat.QR_CODE, side, side,
      mapOf(EncodeHintType.ERROR_CORRECTION to ErrorCorrectionLevel.M))
    val width = matrix.width
    val height = matrix.height
    val pixels = IntArray(width * height)
    for (y in 0 until height) {
      for (x in 0 until width) pixels[y * width + x] = if (matrix[x, y]) Color.BLACK else Color.WHITE
    }
    return Bitmap.createBitmap(pixels, width, height, Bitmap.Config.ARGB_8888)
  }

  fun read(file: File): String {
    require(file.length() in 1..MAX_PHOTO_BYTES)
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeFile(file.path, bounds)
    val longest = maxOf(bounds.outWidth, bounds.outHeight)
    require(bounds.outWidth > 0 && bounds.outHeight > 0)
    var sample = 1
    while ((longest.toLong() + sample - 1) / sample > MAX_PHOTO_EDGE) sample *= 2
    val options = BitmapFactory.Options().apply {
      inSampleSize = sample
      inPreferredConfig = Bitmap.Config.RGB_565
    }
    val image = requireNotNull(BitmapFactory.decodeFile(file.path, options))
    try {
      val width = image.width
      val height = image.height
      require(width in 1..MAX_PHOTO_EDGE && height in 1..MAX_PHOTO_EDGE)
      val pixels = IntArray(width * height)
      image.getPixels(pixels, 0, width, 0, 0, width, height)
      val source = RGBLuminanceSource(width, height, pixels)
      val qr = QRCodeReader().decode(BinaryBitmap(HybridBinarizer(source)),
        mapOf(DecodeHintType.TRY_HARDER to true))
      return qr.text.also { require(isValid(it)) }
    } finally {
      image.recycle()
    }
  }
}
