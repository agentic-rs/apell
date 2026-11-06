package dev.apell.audio

import android.Manifest
import android.app.Activity
import android.app.AlertDialog
import android.content.ClipData
import android.content.Intent
import android.media.MediaPlayer
import android.media.MediaRecorder
import android.os.SystemClock
import androidx.activity.result.ActivityResult
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.FileProvider
import androidx.lifecycle.Lifecycle
import app.tauri.PermissionState
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.Permission
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File
import java.util.concurrent.Executors

@InvokeArg class PathArgs { lateinit var path: String }
@InvokeArg class StartArgs {
  lateinit var path: String
  lateinit var applet_name: String
}
@InvokeArg class ImportArgs { lateinit var directory: String }
@InvokeArg class ShareArgs {
  lateinit var path: String
  lateinit var file_name: String
  lateinit var mime_type: String
}
@InvokeArg class SeekArgs { var position_ms: Double = 0.0 }

@TauriPlugin(permissions = [Permission(strings = [Manifest.permission.RECORD_AUDIO], alias = "microphone")])
class AudioPlugin(private val activity: Activity): Plugin(activity) {
  private data class PendingStart(val invoke: Invoke, val path: String)

  private val closed_sessions = mutableSetOf<String>()
  private var recorder: MediaRecorder? = null
  private var recording_file: File? = null
  private var player: MediaPlayer? = null
  private var player_path: String? = null
  private var pending_play: Invoke? = null
  private var recording_state = "idle"
  private var accumulated_ms = 0L
  private var started_at = 0L
  private var pending_start: PendingStart? = null
  private var start_confirmation: AlertDialog? = null
  private var pending_share: Invoke? = null
  private var pending_import: Invoke? = null
  private var import_directory: String? = null
  @Volatile private var generation = 0
  private val worker = Executors.newSingleThreadExecutor()

  private fun elapsed(): Long = accumulated_ms + if (recording_state == "recording") SystemClock.elapsedRealtime() - started_at else 0

  private fun isVisible(): Boolean =
    (activity as? AppCompatActivity)?.lifecycle?.currentState?.isAtLeast(Lifecycle.State.STARTED) == true

  @Command fun start(invoke: Invoke) {
    val args = invoke.parseArgs(StartArgs::class.java)
    activity.runOnUiThread {
      if (!sessionOpen(invoke)) return@runOnUiThread
      if (!isVisible()) { invoke.reject("Open Apell to start recording."); return@runOnUiThread }
      if (recorder != null || pending_start != null) { invoke.reject("A recording is already active."); return@runOnUiThread }
      pending_start = PendingStart(invoke, args.path)
      try {
        val dialog = AlertDialog.Builder(activity)
          .setTitle("Use microphone?")
          .setMessage("${args.applet_name} wants to start a recording.")
          .setNegativeButton("Cancel") { _, _ -> cancelStart(invoke, "Microphone recording was cancelled.") }
          .setPositiveButton("Start recording") { _, _ -> requestMicrophone(invoke) }
          .setOnCancelListener { cancelStart(invoke, "Microphone recording was cancelled.") }
          .create()
        start_confirmation = dialog
        dialog.show()
      } catch (_: Exception) {
        cancelStart(invoke, "The microphone confirmation is unavailable.")
      }
    }
  }

  private fun cancelStart(invoke: Invoke, reason: String) {
    if (pending_start?.invoke !== invoke) return
    pending_start = null
    val dialog = start_confirmation
    start_confirmation = null
    runCatching { dialog?.dismiss() }
    invoke.reject(reason)
  }

  private fun requestMicrophone(invoke: Invoke) {
    if (pending_start?.invoke !== invoke) return
    start_confirmation = null
    if (!isVisible()) { cancelStart(invoke, "Open Apell to start recording."); return }
    try {
      if (getPermissionState("microphone") != PermissionState.GRANTED) {
        requestPermissionForAlias("microphone", invoke, "microphoneResult")
      } else startGranted(invoke)
    } catch (_: Exception) {
      cancelStart(invoke, "Microphone access could not be requested.")
    }
  }

  @PermissionCallback fun microphoneResult(invoke: Invoke) {
    activity.runOnUiThread {
      if (pending_start?.invoke !== invoke) return@runOnUiThread
      if (getPermissionState("microphone") != PermissionState.GRANTED) {
        cancelStart(invoke, "Microphone access is denied. Enable it for Apell in Settings.")
      } else startGranted(invoke)
    }
  }

  @Suppress("DEPRECATION")
  private fun startGranted(invoke: Invoke) {
    val request = pending_start?.takeIf { it.invoke === invoke } ?: return
    if (!isVisible()) { cancelStart(invoke, "Open Apell to start recording."); return }
    pending_start = null
    if (recorder != null) { invoke.reject("A recording is already active."); return }
    var next: MediaRecorder? = null
    try {
      releasePlayer()
      val created = MediaRecorder()
      next = created
      created.setAudioSource(MediaRecorder.AudioSource.MIC)
      created.setOutputFormat(MediaRecorder.OutputFormat.MPEG_4)
      created.setAudioEncoder(MediaRecorder.AudioEncoder.AAC)
      created.setAudioSamplingRate(44100)
      created.setAudioChannels(1)
      created.setAudioEncodingBitRate(64000)
      created.setOutputFile(request.path)
      created.prepare()
      created.start()
      recorder = created
      recording_file = File(request.path)
      accumulated_ms = 0
      started_at = SystemClock.elapsedRealtime()
      recording_state = "recording"
      invoke.resolve()
    } catch (_: Exception) {
      runCatching { next?.release() }
      File(request.path).delete()
      invoke.reject("Could not start the microphone recording.")
    }
  }

  @Command fun pause(invoke: Invoke) {
    activity.runOnUiThread {
      if (!sessionOpen(invoke)) return@runOnUiThread
      if (recording_state != "recording") { invoke.reject("No recording is running."); return@runOnUiThread }
      try { pauseRecorder(); invoke.resolve() }
      catch (_: Exception) { invoke.reject("Could not pause the recording.") }
    }
  }

  private fun pauseRecorder() {
    val duration = elapsed()
    recorder?.pause()
    accumulated_ms = duration
    recording_state = "paused"
  }

  @Command fun resume(invoke: Invoke) {
    activity.runOnUiThread {
      if (!sessionOpen(invoke)) return@runOnUiThread
      if (!isVisible()) { invoke.reject("Open Apell to resume recording."); return@runOnUiThread }
      if (recording_state != "paused") { invoke.reject("No recording is paused."); return@runOnUiThread }
      try {
        recorder?.resume()
        started_at = SystemClock.elapsedRealtime()
        recording_state = "recording"
        invoke.resolve()
      } catch (_: Exception) { invoke.reject("Could not resume the recording.") }
    }
  }

  @Command fun stop(invoke: Invoke) {
    activity.runOnUiThread {
      if (!sessionOpen(invoke)) return@runOnUiThread
      if (recorder == null) { invoke.reject("No recording is active."); return@runOnUiThread }
      if (finishRecording()) {
        invoke.resolve(JSObject().put("duration_ms", accumulated_ms))
      } else {
        invoke.reject("The recording was too short or could not be saved.")
      }
    }
  }

  @Command fun importAudio(invoke: Invoke) {
    val args = invoke.parseArgs(ImportArgs::class.java)
    activity.runOnUiThread {
      if (!sessionOpen(invoke)) return@runOnUiThread
      if (recorder != null || pending_import != null) {
        invoke.reject("Finish the current audio operation first."); return@runOnUiThread
      }
      pending_import = invoke
      import_directory = args.directory
      try {
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
          addCategory(Intent.CATEGORY_OPENABLE)
          type = "audio/*"
          putExtra(Intent.EXTRA_ALLOW_MULTIPLE, false)
        }
        startActivityForResult(invoke, intent, "importResult")
      } catch (_: Exception) {
        pending_import = null
        import_directory = null
        invoke.reject("The audio picker is unavailable.")
      }
    }
  }

  @ActivityCallback fun importResult(invoke: Invoke, result: ActivityResult) {
    if (pending_import !== invoke) return
    val directory = import_directory
    val uri = result.data?.data
    if (result.resultCode != Activity.RESULT_OK || uri == null || directory == null) {
      pending_import = null
      import_directory = null
      invoke.resolve()
      return
    }
    val current_generation = generation
    worker.execute {
      try {
        val asset = AudioFiles.importAudio(activity, uri, File(directory)) { generation != current_generation }
        activity.runOnUiThread {
          if (generation != current_generation || pending_import !== invoke) {
            File(asset.path).delete()
          } else {
            pending_import = null
            import_directory = null
            invoke.resolveObject(asset)
          }
        }
      } catch (_: Exception) {
        activity.runOnUiThread {
          if (pending_import === invoke) {
            pending_import = null
            import_directory = null
            invoke.reject("Could not import audio. Choose a supported audio file smaller than 128 MiB.")
          }
        }
      }
    }
  }

  @Command fun share(invoke: Invoke) {
    val args = invoke.parseArgs(ShareArgs::class.java)
    activity.runOnUiThread {
      if (!sessionOpen(invoke)) return@runOnUiThread
      if (recorder != null) { invoke.reject("Finish recording before sharing."); return@runOnUiThread }
      if (pending_share != null) { invoke.reject("A share sheet is already opening."); return@runOnUiThread }
      pending_share = invoke
      val current_generation = generation
      worker.execute {
        try {
          val shared = AudioFiles.prepareShare(activity, File(args.path), args.file_name)
          activity.runOnUiThread {
            if (generation != current_generation || pending_share !== invoke) {
              shared.delete()
              return@runOnUiThread
            }
            pending_share = null
            try {
              val uri = FileProvider.getUriForFile(activity, activity.packageName + ".apell.audio.share", shared)
              val intent = Intent(Intent.ACTION_SEND).apply {
                type = args.mime_type
                putExtra(Intent.EXTRA_STREAM, uri)
                clipData = ClipData.newRawUri("Audio", uri)
                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
              }
              activity.startActivity(Intent.createChooser(intent, "Share recording"))
              // Android does not provide reliable completion of the receiving app.
              // Keep the scoped cache copy for a day so recipients can finish reading.
              invoke.resolve()
            } catch (_: Exception) { shared.delete(); invoke.reject("The share sheet is unavailable.") }
          }
        } catch (_: Exception) {
          activity.runOnUiThread {
            if (pending_share === invoke) {
              pending_share = null
              invoke.reject("Could not prepare the file for sharing.")
            }
          }
        }
      }
    }
  }

  @Command fun play(invoke: Invoke) {
    val args = invoke.parseArgs(PathArgs::class.java)
    activity.runOnUiThread {
      if (!sessionOpen(invoke)) return@runOnUiThread
      if (recorder != null) { invoke.reject("Finish recording before playback."); return@runOnUiThread }
      try {
        val current = player
        if (player_path == args.path && current != null) {
          if (current.currentPosition >= current.duration) current.seekTo(0)
          current.start()
          invoke.resolve()
          return@runOnUiThread
        }
        releasePlayer()
        val next = MediaPlayer()
        player = next
        player_path = args.path
        next.setDataSource(args.path)
        pending_play = invoke
        next.setOnPreparedListener {
          if (pending_play === invoke && player === next) {
            pending_play = null
            try { it.start(); invoke.resolve() }
            catch (_: Exception) { releasePlayer(); invoke.reject("This audio file could not be played.") }
          }
        }
        next.setOnErrorListener { _, _, _ ->
          if (pending_play === invoke) {
            pending_play = null
            invoke.reject("This audio file could not be played.")
          }
          releasePlayer()
          true
        }
        next.prepareAsync()
      } catch (_: Exception) {
        pending_play = null
        releasePlayer()
        invoke.reject("This audio file could not be played.")
      }
    }
  }

  @Command fun playbackPause(invoke: Invoke) {
    activity.runOnUiThread {
      if (!sessionOpen(invoke)) return@runOnUiThread
      try { if (player?.isPlaying == true) player?.pause(); invoke.resolve() }
      catch (_: Exception) { invoke.reject("Could not pause playback.") }
    }
  }

  @Suppress("DEPRECATION")
  @Command fun seek(invoke: Invoke) {
    val args = invoke.parseArgs(SeekArgs::class.java)
    activity.runOnUiThread {
      if (!sessionOpen(invoke)) return@runOnUiThread
      val current = player
      if (current == null || !args.position_ms.isFinite()) { invoke.reject("Load an audio file before seeking."); return@runOnUiThread }
      try {
        current.seekTo(args.position_ms.coerceIn(0.0, current.duration.toDouble()).toInt())
        invoke.resolve()
      } catch (_: Exception) { invoke.reject("Could not seek in the audio file.") }
    }
  }

  @Command fun status(invoke: Invoke) {
    activity.runOnUiThread {
      if (!sessionOpen(invoke)) return@runOnUiThread
      try {
        invoke.resolve(JSObject().apply {
          put("recording_state", recording_state)
          put("elapsed_ms", elapsed())
          put("playing", player?.isPlaying ?: false)
          put("position_ms", player?.currentPosition ?: 0)
          put("duration_ms", player?.duration ?: 0)
        })
      } catch (_: Exception) { invoke.reject("Audio status is temporarily unavailable.") }
    }
  }

  @Command fun reset(invoke: Invoke) {
    val session_id = invoke.getArgs().getString("session_id", null)
    if (session_id.isNullOrBlank()) { invoke.reject("An audio session is required."); return }
    activity.runOnUiThread {
      closed_sessions.add(session_id)
      releaseAll()
      invoke.resolve()
    }
  }

  private fun sessionOpen(invoke: Invoke): Boolean {
    val session_id = invoke.getArgs().getString("session_id", null)
    if (session_id.isNullOrBlank()) { invoke.reject("An audio session is required."); return false }
    if (closed_sessions.contains(session_id)) { invoke.reject("The audio session closed."); return false }
    return true
  }

  private fun releasePlayer() {
    pending_play?.reject("Audio playback was cancelled.")
    pending_play = null
    player?.release()
    player = null
    player_path = null
  }

  private fun finishRecording(): Boolean {
    val current = recorder ?: return false
    val output = recording_file
    accumulated_ms = elapsed()
    recorder = null
    recording_file = null
    recording_state = "idle"
    var finalized = false
    try {
      current.stop()
      finalized = true
    } catch (_: Exception) {
      accumulated_ms = 0
    } finally {
      // stop() can fail before AAC has produced a valid container. Release the
      // writer before removing that file; a successful stop remains recoverable.
      runCatching { current.release() }
      if (!finalized) output?.delete()
    }
    return finalized
  }

  private fun releaseAll() {
    generation += 1
    pending_start?.let { cancelStart(it.invoke, "The audio session closed.") }
    pending_share?.resolve()
    pending_share = null
    finishRecording()
    recording_state = "idle"
    accumulated_ms = 0
    releasePlayer()
    pending_import?.resolve()
    pending_import = null
    import_directory = null
  }

  override fun onStop(activity: AppCompatActivity) {
    pending_start?.let { cancelStart(it.invoke, "Open Apell to start recording.") }
    // Background capture is intentionally unsupported without a foreground
    // service. Keep the partial recording and make the paused state explicit.
    if (recording_state == "recording") {
      try { pauseRecorder() } catch (_: Exception) { releaseAll() }
    }
    try { if (player?.isPlaying == true) player?.pause() } catch (_: Exception) {}
  }

  override fun onDestroy(activity: AppCompatActivity) { releaseAll(); worker.shutdown() }
}
