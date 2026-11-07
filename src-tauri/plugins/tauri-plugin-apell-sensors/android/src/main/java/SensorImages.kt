package dev.apell.sensors

import android.content.ContentResolver
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Matrix
import android.media.ExifInterface
import android.net.Uri
import android.os.CancellationSignal
import android.os.OperationCanceledException
import java.io.File
import java.io.InputStream
import java.io.OutputStream
import java.util.UUID
import java.util.concurrent.atomic.AtomicReference

internal class PhotoRead(private val resolver: ContentResolver, private val uri: Uri) {
  private val cancellation = CancellationSignal()
  private val opened = AtomicReference<InputStream?>()

  fun open(): InputStream {
    cancellation.throwIfCanceled()
    val descriptor = resolver.openAssetFileDescriptor(uri, "r", cancellation)
      ?: error("Photo is unavailable.")
    val input = try {
      descriptor.createInputStream()
    } catch (error: Exception) {
      descriptor.close()
      throw error
    }
    opened.set(input)
    if (cancellation.isCanceled) {
      close()
      throw OperationCanceledException()
    }
    return input
  }

  fun close() { opened.getAndSet(null)?.close() }

  fun cancel() {
    // Cancelling the provider request handles a blocked open. Closing the
    // stream handles a provider that already returned a streaming descriptor.
    try { close() } finally { cancellation.cancel() }
  }
}

internal object SensorImages {
  // Bound the pixel allocation separately from the compressed input and output sizes.
  private const val MAX_IMAGE_PIXELS = 12_500_000L
  private const val MAX_IMAGE_EDGE = 4096L

  fun copyPhoto(
    source: PhotoRead,
    target: File,
    max_bytes: Long,
    active: () -> Boolean,
  ) {
    check(active())
    val input = source.open()
    try {
      writeImage(target, max_bytes, active) { output ->
        val buffer = ByteArray(64 * 1024)
        while (true) {
          check(active())
          val length = input.read(buffer)
          if (length < 0) break
          output.write(buffer, 0, length)
        }
      }
    } finally { source.close() }
  }

  fun encodeCapture(source: File, target: File, max_bytes: Long, active: () -> Boolean) {
    encodeImage(source, target, max_bytes, active, require_jpeg = true)
  }

  fun convertPhoto(source: PhotoRead, target: File, max_bytes: Long, active: () -> Boolean) {
    check(active())
    // Keep the original beside Rust's staged output, outside applet storage.
    // Opening a fresh file cannot recreate a directory removed on cancellation.
    val original = File(target.parentFile, ".photo-${UUID.randomUUID()}.source")
    try {
      copyPhoto(source, original, max_bytes, active)
      encodeImage(original, target, max_bytes, active, require_jpeg = false)
    } finally {
      original.delete()
    }
  }

  private fun encodeImage(
    source: File,
    target: File,
    max_bytes: Long,
    active: () -> Boolean,
    require_jpeg: Boolean,
  ) {
    check(active())
    require(source.length() in 1..max_bytes)
    val options = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeFile(source.path, options)
    require(options.outWidth > 0 && options.outHeight > 0)
    if (require_jpeg) require(options.outMimeType == "image/jpeg")
    var sample = 1
    while (sampled(options.outWidth, sample) > MAX_IMAGE_EDGE ||
      sampled(options.outHeight, sample) > MAX_IMAGE_EDGE ||
      sampled(options.outWidth, sample) * sampled(options.outHeight, sample) > MAX_IMAGE_PIXELS
    ) sample *= 2
    val orientation = if (require_jpeg) {
      readOrientation(source)
    } else {
      // Some decodable formats (such as GIF) have no EXIF metadata.
      runCatching { readOrientation(source) }.getOrDefault(ExifInterface.ORIENTATION_NORMAL)
    }
    check(active())
    val bitmap = BitmapFactory.decodeFile(source.path, BitmapFactory.Options().apply { inSampleSize = sample })
      ?: error("The selected photo is not a supported image.")
    try {
      check(active())
      require(bitmap.width.toLong() <= MAX_IMAGE_EDGE && bitmap.height.toLong() <= MAX_IMAGE_EDGE &&
        bitmap.width.toLong() * bitmap.height <= MAX_IMAGE_PIXELS)
      val matrix = orientationMatrix(orientation)
      val upright = Bitmap.createBitmap(bitmap, 0, 0, bitmap.width, bitmap.height, matrix, true)
      try {
        // Encoding decoded pixels keeps the visual orientation and drops source metadata.
        writeImage(target, max_bytes, active) { output ->
          check(upright.compress(Bitmap.CompressFormat.JPEG, 90, output))
        }
      } finally {
        if (upright !== bitmap) upright.recycle()
      }
    } finally {
      bitmap.recycle()
    }
  }

  private fun readOrientation(source: File): Int = ExifInterface(source.path).getAttributeInt(
    ExifInterface.TAG_ORIENTATION,
    ExifInterface.ORIENTATION_NORMAL,
  )

  private fun sampled(edge: Int, sample: Int): Long = (edge.toLong() + sample - 1) / sample

  private fun orientationMatrix(orientation: Int): Matrix = Matrix().apply {
    when (orientation) {
      ExifInterface.ORIENTATION_FLIP_HORIZONTAL -> setScale(-1f, 1f)
      ExifInterface.ORIENTATION_ROTATE_180 -> setRotate(180f)
      ExifInterface.ORIENTATION_FLIP_VERTICAL -> setScale(1f, -1f)
      ExifInterface.ORIENTATION_TRANSPOSE -> { setRotate(90f); postScale(-1f, 1f) }
      ExifInterface.ORIENTATION_ROTATE_90 -> setRotate(90f)
      ExifInterface.ORIENTATION_TRANSVERSE -> { setRotate(-90f); postScale(-1f, 1f) }
      ExifInterface.ORIENTATION_ROTATE_270 -> setRotate(-90f)
    }
  }

  private fun writeImage(target: File, limit: Long, active: () -> Boolean, write: (OutputStream) -> Unit) {
    check(active())
    // Rust owns the staging directory. A cancelled request must not recreate it.
    require(target.createNewFile())
    try {
      target.outputStream().use { file ->
        val output = BoundedImageOutput(file, limit, active)
        write(output)
        check(active() && output.size > 0)
      }
    } catch (error: Exception) {
      target.delete()
      throw error
    }
  }

  private class BoundedImageOutput(
    private val output: OutputStream,
    private val limit: Long,
    private val active: () -> Boolean,
  ) : OutputStream() {
    var size = 0L
      private set

    override fun write(byte: Int) {
      reserve(1)
      output.write(byte)
    }

    override fun write(bytes: ByteArray, offset: Int, length: Int) {
      reserve(length)
      output.write(bytes, offset, length)
    }

    private fun reserve(length: Int) {
      check(active() && length <= limit - size)
      size += length
    }
  }
}
