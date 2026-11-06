package dev.apell.audio

import android.app.Activity
import android.media.MediaMetadataRetriever
import android.net.Uri
import android.provider.OpenableColumns
import android.webkit.MimeTypeMap
import java.io.File
import java.util.UUID

// Field names are the wire contract shared with Rust and the applet SDK.
data class ImportedAudio(
  val path: String,
  val file_name: String,
  val mime_type: String,
  val duration_ms: Long,
  val size_bytes: Long,
)

internal object AudioFiles {
  private const val MAX_BYTES = 2L * 1024 * 1024 * 1024

  fun importAudio(activity: Activity, uri: Uri, directory: File, cancelled: () -> Boolean): ImportedAudio {
    val resolver = activity.contentResolver
    val mime = resolver.getType(uri) ?: "application/octet-stream"
    val display_name = resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use {
      if (it.moveToFirst()) it.getString(0) else null
    } ?: "Imported audio"
    val candidate = display_name.substringAfterLast('.', "").lowercase()
    val ext = if (candidate.matches(Regex("[a-z0-9]{1,8}"))) candidate
      else MimeTypeMap.getSingleton().getExtensionFromMimeType(mime) ?: "audio"
    val target = File(directory, UUID.randomUUID().toString() + "." + ext)
    try {
      val size = resolver.openInputStream(uri)!!.use { input ->
        target.outputStream().use { output ->
          val buffer = ByteArray(64 * 1024)
          var count = 0L
          while (true) {
            check(!cancelled())
            val next = input.read(buffer)
            if (next < 0) break
            count += next
            require(count <= MAX_BYTES)
            output.write(buffer, 0, next)
          }
          require(count > 0)
          count
        }
      }
      val metadata = MediaMetadataRetriever()
      val duration = try {
        metadata.setDataSource(target.absolutePath)
        require(metadata.extractMetadata(MediaMetadataRetriever.METADATA_KEY_HAS_AUDIO) == "yes")
        metadata.extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION)?.toLongOrNull() ?: 0
      } finally { metadata.release() }
      require(duration in 1..(3 * 60 * 60 * 1000L))
      return ImportedAudio(target.absolutePath, File(display_name).name, mime, duration, size)
    } catch (error: Exception) {
      target.delete()
      throw error
    }
  }

  fun prepareShare(activity: Activity, source: File, file_name: String, cancelled: () -> Boolean): File {
    require(source.isFile && source.length() <= MAX_BYTES)
    val name = File(file_name).name
    require(name.isNotBlank() && name != "." && name != "..")
    val root = File(activity.cacheDir, "apell-audio-share").apply { mkdirs() }
    val cutoff = System.currentTimeMillis() - 24 * 60 * 60 * 1000L
    root.listFiles()?.filter { it.lastModified() < cutoff }?.forEach { it.deleteRecursively() }
    val folder = File(root, UUID.randomUUID().toString()).apply { mkdir() }
    val target = File(folder, name)
    try {
      source.inputStream().use { input -> target.outputStream().use { output ->
        val buffer = ByteArray(64 * 1024)
        var total = 0L
        while (true) {
          check(!cancelled())
          val count = input.read(buffer)
          if (count < 0) break
          total += count
          require(total <= MAX_BYTES)
          output.write(buffer, 0, count)
        }
      } }
      return target
    }
    catch (error: Exception) { folder.deleteRecursively(); throw error }
  }
}
