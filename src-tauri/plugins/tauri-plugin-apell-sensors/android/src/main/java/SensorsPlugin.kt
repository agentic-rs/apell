package dev.apell.sensors

import android.Manifest
import android.app.Activity
import android.app.AlertDialog
import android.content.ClipData
import android.content.ContentResolver
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.provider.MediaStore
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContract
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat
import androidx.core.content.FileProvider
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File
import java.util.UUID
import java.util.concurrent.Executors
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.SynchronousQueue
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit

@InvokeArg class SensorRunArgs {
  var session_id: String = ""
  var operation_id: String = ""
  var kind: String = ""
  var applet_name: String = ""
  var remembered: Boolean = false
  var staging_path: String? = null
  var max_image_bytes: Long = 0
}

@InvokeArg class SensorCancelArgs {
  var session_id: String = ""
  var operation_id: String = ""
}

@InvokeArg class TransferQrArgs {
  var payload: String = ""
}

@TauriPlugin
class SensorsPlugin(private val activity: Activity) : Plugin(activity) {
  private data class OperationId(val session_id: String, val operation_id: String) {
    fun isValid() = session_id.isNotBlank() && operation_id.isNotBlank()
  }

  private class Operation(
    val id: OperationId,
    val args: SensorRunArgs,
    val invoke: Invoke,
    val transfer_qr: Boolean = false,
  ) {
    val target = args.staging_path?.let(::File)
    var remember = false
    var dialog: AlertDialog? = null
    var launcher: ActivityResultLauncher<*>? = null
    var resume_location: LifecycleEventObserver? = null
    var location: SensorLocation? = null
    var photo: PhotoRead? = null
    var camera_file: File? = null
    var camera_uri: Uri? = null
    @Volatile var active = true
  }

  private val host get() = activity as? AppCompatActivity
  private val main = Handler(Looper.getMainLooper())
  // A provider that ignores cancellation cannot indefinitely queue a newer
  // request. Permit one stale transfer, then report busy if both workers stall.
  private val worker = ThreadPoolExecutor(0, 2, 30L, TimeUnit.SECONDS, SynchronousQueue())
  private val photo_cancellations = Executors.newCachedThreadPool()
  private val launchers = mutableSetOf<ActivityResultLauncher<*>>()
  private var pending: Operation? = null
  private var cancelled_before_run: OperationId? = null
  private var transferQrDisplay: AlertDialog? = null
  private val transferFile = TransferFilePicker(activity)

  @Command fun run(invoke: Invoke) {
    val args = invoke.parseArgs(SensorRunArgs::class.java)
    val id = OperationId(args.session_id, args.operation_id)
    activity.runOnUiThread {
      if (!id.isValid() || args.kind !in setOf("camera", "photos", "location")) {
        invoke.reject("The sensor request is invalid.")
        return@runOnUiThread
      }
      if (cancelled_before_run == id) {
        cancelled_before_run = null
        invoke.resolve(cancelled())
        return@runOnUiThread
      }
      if (pending != null) {
        invoke.reject("Finish the current camera, photo, or location request first.")
        return@runOnUiThread
      }
      if (!isVisible()) {
        invoke.reject("Open Apell to use the camera, photo picker, or location.")
        return@runOnUiThread
      }
      val operation = Operation(id, args, invoke)
      if (args.kind != "location" && (operation.target?.isAbsolute != true || args.max_image_bytes <= 0)) {
        invoke.reject("The image request has no private destination.")
        return@runOnUiThread
      }
      pending = operation
      if (args.kind == "photos" || args.remembered) start(operation) else confirm(operation)
    }
  }

  @Command fun cancel(invoke: Invoke) {
    val args = invoke.parseArgs(SensorCancelArgs::class.java)
    val id = OperationId(args.session_id, args.operation_id)
    activity.runOnUiThread {
      if (!id.isValid()) {
        invoke.reject("The sensor request is invalid.")
        return@runOnUiThread
      }
      val operation = pending
      if (operation?.id == id) finish(operation, cancelled()) else cancelled_before_run = id
      invoke.resolve()
    }
  }

  @Command fun showTransferQr(invoke: Invoke) {
    val args = invoke.parseArgs(TransferQrArgs::class.java)
    activity.runOnUiThread {
      if (!TransferQr.isValid(args.payload)) {
        invoke.reject("The settings QR code is invalid.")
        return@runOnUiThread
      }
      if (!isVisible() || pending != null || transferQrDisplay != null) {
        invoke.reject("Open Apell in the foreground to show the settings QR code.")
        return@runOnUiThread
      }
      try {
        val dialog = TransferQr.dialog(activity, args.payload) {
          transferQrDisplay = null
          invoke.resolve()
        }
        dialog.show()
        transferQrDisplay = dialog
      } catch (_: Exception) {
        invoke.reject("Could not display the settings QR code.")
      }
    }
  }

  @Command fun scanTransferQr(invoke: Invoke) {
    activity.runOnUiThread {
      if (!isVisible() || pending != null || transferQrDisplay != null) {
        invoke.reject("Open Apell in the foreground to scan a settings QR code.")
        return@runOnUiThread
      }
      // Reuse the permissionless system camera. Declaring CAMERA for a
      // separate scanner would make delegated capture require it too.
      val operation = Operation(
        OperationId("transfer", UUID.randomUUID().toString()), SensorRunArgs(), invoke, transfer_qr = true
      )
      pending = operation
      openCamera(operation)
    }
  }

  @Command fun saveTransferFile(invoke: Invoke) = transferFile.save(invoke)

  private fun confirm(operation: Operation) {
    val camera = operation.args.kind == "camera"
    val title = if (camera) "Use camera?" else "Share current location?"
    val action = if (camera) "take a photo" else "get your current location"
    try {
      val dialog = AlertDialog.Builder(activity)
        .setTitle(title)
        .setMessage("${operation.args.applet_name} wants to $action.")
        .setNegativeButton("Cancel") { _, _ -> finish(operation, cancelled()) }
        .setNeutralButton("Always allow") { _, _ -> start(operation, remember = true) }
        .setPositiveButton("Allow once") { _, _ -> start(operation) }
        .setOnCancelListener { finish(operation, cancelled()) }
        .create()
      operation.dialog = dialog
      dialog.show()
    } catch (_: Exception) {
      fail(operation, "The permission confirmation is unavailable.")
    }
  }

  private fun start(operation: Operation, remember: Boolean = false) {
    if (!operation.active) return
    operation.dialog = null
    operation.remember = remember
    if (!isVisible()) {
      finish(operation, cancelled())
      return
    }
    when (operation.args.kind) {
      "camera" -> openCamera(operation)
      "photos" -> openPhotos(operation)
      "location" -> requestLocationPermission(operation)
    }
  }

  private fun openCamera(operation: Operation) {
    try {
      val directory = File(activity.cacheDir, "apell-camera")
      check(directory.isDirectory || directory.mkdir())
      val cutoff = System.currentTimeMillis() - 24 * 60 * 60 * 1000L
      directory.listFiles()?.filter { it.lastModified() < cutoff }?.forEach { it.delete() }
      val source = File.createTempFile("capture-", ".jpg", directory)
      operation.camera_file = source
      val uri = FileProvider.getUriForFile(activity, "${activity.packageName}.apell.sensors.camera", source)
      operation.camera_uri = uri
      val intent = Intent(MediaStore.ACTION_IMAGE_CAPTURE).apply {
        putExtra(MediaStore.EXTRA_OUTPUT, uri)
        clipData = ClipData.newRawUri("Apell camera output", uri)
        addFlags(CAMERA_URI_FLAGS)
      }
      launch(operation, ActivityResultContracts.StartActivityForResult(), intent) { result ->
        revokeCameraUri(operation)
        if (result.resultCode != Activity.RESULT_OK) {
          finish(operation, if (operation.transfer_qr) qrCancelled() else cancelled())
        } else if (operation.transfer_qr) {
          decodeTransferQr(operation, source)
        } else {
          writeImage(operation, "Could not prepare the captured photo.") { target, limit ->
            SensorImages.encodeCapture(source, target, limit) { operation.active }
          }
        }
      }
    } catch (_: Exception) {
      fail(operation, "The camera is unavailable.")
    }
  }

  private fun decodeTransferQr(operation: Operation, source: File) {
    try {
      worker.execute {
        val payload = runCatching { TransferQr.read(source) }.getOrNull()
        activity.runOnUiThread {
          if (!operation.active) return@runOnUiThread
          if (payload == null) {
            fail(operation, "Could not read an Apell settings QR code. Try taking a closer photo.")
          } else {
            finish(operation, JSObject().put("payload", payload))
          }
        }
      }
    } catch (_: RejectedExecutionException) {
      fail(operation, "Previous image transfers are still stopping. Try again shortly.")
    }
  }

  private fun openPhotos(operation: Operation) {
    try {
      val selection = PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly)
      launch(operation, ActivityResultContracts.PickVisualMedia(), selection) { uri ->
        if (uri == null) {
          finish(operation, cancelled())
        } else if (!isPhotoContent(uri)) {
          fail(operation, "The selected photo is unavailable.")
        } else {
          val photo = PhotoRead(activity.contentResolver, uri)
          operation.photo = photo
          writeImage(operation, "Could not import the selected photo within the image size limit.") { target, limit ->
            SensorImages.copyPhoto(photo, target, limit) { operation.active }
          }
        }
      }
    } catch (_: Exception) {
      fail(operation, "The photo picker is unavailable.")
    }
  }

  private fun isPhotoContent(uri: Uri): Boolean {
    val authority = uri.authority ?: return false
    return uri.scheme == ContentResolver.SCHEME_CONTENT && !authority.startsWith("${activity.packageName}.apell.")
  }

  private fun writeImage(operation: Operation, error: String, write: (File, Long) -> Unit) {
    val target = checkNotNull(operation.target)
    try {
      worker.execute {
        val success = runCatching { write(target, operation.args.max_image_bytes) }.isSuccess
        activity.runOnUiThread {
          operation.photo = null
          if (!operation.active) {
            target.delete()
          } else if (success) {
            finish(operation, JSObject().put("status", "image").put("remember", operation.remember))
          } else {
            fail(operation, error)
          }
        }
      }
    } catch (_: RejectedExecutionException) {
      operation.photo = null
      fail(operation, "Previous image transfers are still stopping. Try again shortly.")
    }
  }

  private fun requestLocationPermission(operation: Operation) {
    if (hasPermission(Manifest.permission.ACCESS_COARSE_LOCATION) || hasPermission(Manifest.permission.ACCESS_FINE_LOCATION)) {
      readLocation(operation)
      return
    }
    try {
      val permissions = arrayOf(Manifest.permission.ACCESS_COARSE_LOCATION, Manifest.permission.ACCESS_FINE_LOCATION)
      launch(operation, ActivityResultContracts.RequestMultiplePermissions(), permissions) { _ ->
        if (isVisible()) readLocation(operation) else readLocationOnStart(operation)
      }
    } catch (_: Exception) {
      fail(operation, "Location permission could not be requested.")
    }
  }

  private fun readLocationOnStart(operation: Operation) {
    val lifecycle = checkNotNull(host).lifecycle
    if (activity.isFinishing || activity.isDestroyed) {
      finish(operation, cancelled())
      return
    }
    val observer = LifecycleEventObserver { _, event ->
      if (event == Lifecycle.Event.ON_START) {
        operation.resume_location?.let(lifecycle::removeObserver)
        operation.resume_location = null
        if (operation.active) readLocation(operation)
      }
    }
    operation.resume_location = observer
    lifecycle.addObserver(observer)
  }

  private fun readLocation(operation: Operation) {
    if (!isVisible()) {
      finish(operation, cancelled())
      return
    }
    val precise = hasPermission(Manifest.permission.ACCESS_FINE_LOCATION)
    if (!precise && !hasPermission(Manifest.permission.ACCESS_COARSE_LOCATION)) {
      fail(operation, "Location access is denied. Enable it for Apell in Android Settings.")
      return
    }
    try {
      val reading = SensorLocation(activity) { location ->
        if (location == null) {
          fail(operation, "A current location could not be determined.")
        } else if (!isVisible()) {
          finish(operation, cancelled())
        } else {
          finish(operation, JSObject()
            .put("status", "location")
            .put("remember", operation.remember)
            .put("latitude", location.latitude)
            .put("longitude", location.longitude)
            .put("accuracy_m", location.accuracy.toDouble())
            .put("timestamp_ms", location.time))
        }
      }
      operation.location = reading
      reading.request(precise)
    } catch (_: Exception) {
      fail(operation, "Location is unavailable. Check that Android location services are enabled.")
    }
  }

  private fun hasPermission(permission: String): Boolean =
    ContextCompat.checkSelfPermission(activity, permission) == PackageManager.PERMISSION_GRANTED

  private fun isVisible(): Boolean = !activity.isFinishing && !activity.isDestroyed &&
    host?.lifecycle?.currentState?.isAtLeast(Lifecycle.State.STARTED) == true

  private fun <I, O> launch(operation: Operation, contract: ActivityResultContract<I, O>, input: I, result: (O) -> Unit) {
    val registry = checkNotNull(host).activityResultRegistry
    // Tauri shares a single callback per result kind across its plugins. Use a
    // dedicated registry key so audio operations cannot replace our callback.
    val launcher = registry.register("apell-sensors:${operation.id.operation_id}", contract) { value ->
      val completed = operation.launcher
      operation.launcher = null
      // Unregister after AndroidX has removed the launched key. A cancelled
      // operation drains its result here too, without delivering it to Rust.
      main.post { completed?.let(::unregister) }
      if (operation.active) result(value)
    }
    operation.launcher = launcher
    launchers.add(launcher)
    try {
      launcher.launch(input)
    } catch (error: Exception) {
      operation.launcher = null
      unregister(launcher)
      throw error
    }
  }

  private fun unregister(launcher: ActivityResultLauncher<*>) {
    if (launchers.remove(launcher)) launcher.unregister()
  }

  private fun revokeCameraUri(operation: Operation) {
    operation.camera_uri?.let { runCatching { activity.revokeUriPermission(it, CAMERA_URI_FLAGS) } }
    operation.camera_uri = null
  }

  private fun fail(operation: Operation, error: String) = finish(operation, null, error)

  private fun finish(operation: Operation, result: JSObject?, error: String? = null) {
    if (pending !== operation) return
    pending = null
    operation.active = false
    runCatching { operation.dialog?.dismiss() }
    operation.dialog = null
    operation.resume_location?.let { host?.lifecycle?.removeObserver(it) }
    operation.resume_location = null
    operation.location?.stop()
    operation.photo?.let { photo -> photo_cancellations.execute { runCatching { photo.cancel() } } }
    operation.photo = null
    revokeCameraUri(operation)
    operation.camera_file?.delete()
    if (error != null) operation.invoke.reject(error) else operation.invoke.resolve(result)
  }

  override fun onStop(activity: AppCompatActivity) {
    if (activity !== this.activity) return
    transferQrDisplay?.dismiss()
    // System camera and photo picker are activities. Going behind them is normal.
    pending?.let { operation ->
      if (operation.dialog != null || (operation.args.kind == "location" && operation.launcher == null)) {
        finish(operation, cancelled())
      }
    }
  }

  override fun onDestroy(activity: AppCompatActivity) {
    if (activity !== this.activity) return
    transferQrDisplay?.dismiss()
    pending?.let { finish(it, if (it.transfer_qr) qrCancelled() else cancelled()) }
    transferFile.destroy()
    launchers.toList().forEach(::unregister)
    worker.shutdown()
    photo_cancellations.shutdown()
  }

  private companion object {
    const val CAMERA_URI_FLAGS = Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
    fun cancelled(): JSObject = JSObject().put("status", "cancelled")
    fun qrCancelled(): JSObject = JSObject().put("payload", org.json.JSONObject.NULL)
  }
}
