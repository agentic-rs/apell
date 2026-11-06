package dev.apell.sensors

import android.app.Activity
import android.os.Handler
import android.os.Looper
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AppCompatActivity
import app.tauri.annotation.InvokeArg
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import java.io.File
import java.util.UUID
import kotlin.concurrent.thread

@InvokeArg class TransferFileArgs {
  var path: String = ""
}

internal class TransferFilePicker(private val activity: Activity) {
  private val main = Handler(Looper.getMainLooper())
  private var pending: Invoke? = null
  private var launcher: ActivityResultLauncher<String>? = null

  fun save(invoke: Invoke) {
    val args = try {
      invoke.parseArgs(TransferFileArgs::class.java)
    } catch (_: Exception) {
      invoke.reject("Could not prepare the settings file.")
      return
    }
    activity.runOnUiThread {
      val host = activity as? AppCompatActivity
      if (pending != null || host == null || host.isFinishing || host.isDestroyed) {
        invoke.reject("Finish the current native dialog first.")
        return@runOnUiThread
      }
      val file = File(args.path)
      pending = invoke
      try {
        val choice = host.activityResultRegistry.register(
          "apell-settings-file:${UUID.randomUUID()}",
          ActivityResultContracts.CreateDocument("application/octet-stream"),
        ) { uri ->
          val completed = launcher
          launcher = null
          // AndroidX releases the launched key after delivering this callback.
          main.post { completed?.unregister() }
          val request = pending ?: return@register
          pending = null
          if (uri == null) {
            request.resolve(JSObject().put("saved", false))
          } else {
            thread(name = "apell-settings-export") {
              val saved = runCatching {
                file.inputStream().use { input ->
                  val output = activity.contentResolver.openOutputStream(uri, "w")
                    ?: error("Could not open the destination.")
                  output.use { input.copyTo(it) }
                }
              }.isSuccess
              activity.runOnUiThread {
                if (saved) request.resolve(JSObject().put("saved", true))
                else request.reject("Could not save the settings file.")
              }
            }
          }
        }
        launcher = choice
        choice.launch("Apell.apell-settings")
      } catch (_: Exception) {
        launcher?.unregister()
        launcher = null
        pending = null
        invoke.reject("Could not open the save dialog.")
      }
    }
  }

  fun destroy() {
    launcher?.unregister()
    launcher = null
    pending?.resolve(JSObject().put("saved", false))
    pending = null
  }
}
