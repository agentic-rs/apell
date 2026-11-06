import AVFoundation
import Foundation
import Tauri
import UIKit

struct SessionArgs: Decodable { let session_id: String }
struct PathArgs: Decodable { let path: String }
struct RecordingArgs: Decodable {
  let path: String
  let applet_name: String
  let remembered: Bool
}
struct ImportArgs: Decodable { let directory: String }
struct ShareArgs: Decodable { let path: String; let file_name: String; let mime_type: String }
struct SeekArgs: Decodable { let position_ms: Double }

final class AudioPlugin: Plugin, AVAudioRecorderDelegate, AVAudioPlayerDelegate {
  var recorder: AVAudioRecorder?
  var player: AVAudioPlayer?
  var recording_state = "idle"
  var last_elapsed_ms: Double = 0
  var picker: AudioPicker?
  var interruption: NSObjectProtocol?
  var background: NSObjectProtocol?
  var closed_sessions = Set<String>()
  var generation = 0
  var pending_start: Invoke?
  var start_prompt: UIAlertController?
  var pending_share: Invoke?
  var share_sheet: UIActivityViewController?
  var share_folder: URL?

  override init() {
    super.init()
    background = NotificationCenter.default.addObserver(
      forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: .main
    ) { [weak self] _ in
      guard let self = self else { return }
      self.cancelPendingStart("Apell must be in the foreground to record.")
      if self.recording_state == "recording" {
        self.recorder?.pause()
        self.recording_state = "paused"
      }
      if let player = self.player {
        player.pause()
        self.deactivateAudioSession()
      }
    }
    interruption = NotificationCenter.default.addObserver(
      forName: AVAudioSession.interruptionNotification, object: nil, queue: .main
    ) { [weak self] notification in
      guard let self = self,
        let type = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt,
        type == AVAudioSession.InterruptionType.began.rawValue else { return }
      if self.recording_state == "recording" {
        self.recorder?.pause()
        self.recording_state = "paused"
      }
      self.player?.pause()
    }
  }

  deinit {
    if let interruption = interruption { NotificationCenter.default.removeObserver(interruption) }
    if let background = background { NotificationCenter.default.removeObserver(background) }
    recorder?.stop()
    player?.stop()
  }

  @objc func start(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(RecordingArgs.self)
    DispatchQueue.main.async {
      guard self.sessionOpen(invoke) else { return }
      guard UIApplication.shared.applicationState == .active else {
        invoke.reject("Apell must be in the foreground to record."); return
      }
      guard self.recorder == nil, self.pending_start == nil else { invoke.reject("A recording is already active."); return }
      let generation = self.generation
      if args.remembered {
        self.pending_start = invoke
        self.requestMicrophone(invoke, path: args.path, generation: generation, remember: false)
        return
      }
      guard let presenter = self.manager.viewController, presenter.presentedViewController == nil else {
        invoke.reject("Microphone confirmation is unavailable."); return
      }
      self.pending_start = invoke
      let prompt = UIAlertController(
        title: "Allow microphone recording?",
        message: "\(args.applet_name) wants to start recording audio.",
        preferredStyle: .alert
      )
      prompt.addAction(UIAlertAction(title: "Cancel", style: .cancel) { [weak self] _ in
        guard let self = self, self.generation == generation, self.pending_start === invoke else { return }
        self.start_prompt = nil
        self.pending_start = nil
        invoke.reject("Microphone recording was cancelled.")
      })
      prompt.addAction(UIAlertAction(title: "Allow Once", style: .default) { [weak self] _ in
        self?.requestMicrophone(invoke, path: args.path, generation: generation, remember: false)
      })
      prompt.addAction(UIAlertAction(title: "Remember for This Applet", style: .default) { [weak self] _ in
        self?.requestMicrophone(invoke, path: args.path, generation: generation, remember: true)
      })
      self.start_prompt = prompt
      presenter.present(prompt, animated: true)
    }
  }

  private func requestMicrophone(_ invoke: Invoke, path: String, generation: Int, remember: Bool) {
    guard self.generation == generation, pending_start === invoke else { return }
    start_prompt = nil
    AVAudioSession.sharedInstance().requestRecordPermission { [weak self] granted in
      DispatchQueue.main.async {
        guard let self = self, self.generation == generation, self.pending_start === invoke else { return }
        self.pending_start = nil
        guard granted else {
          invoke.reject("Microphone access is denied. Enable it for Apell in Settings.")
          return
        }
        // The system permission alert can leave the app temporarily inactive.
        guard UIApplication.shared.applicationState != .background else {
          invoke.reject("Apell must be in the foreground to record.")
          return
        }
        self.startRecording(invoke, path: path, remember: remember)
      }
    }
  }

  private func cancelPendingStart(_ message: String) {
    let pending = pending_start
    pending_start = nil
    start_prompt?.dismiss(animated: true)
    start_prompt = nil
    pending?.reject(message)
  }

  private func startRecording(_ invoke: Invoke, path: String, remember: Bool) {
    do {
      player?.stop()
      player = nil
      let session = AVAudioSession.sharedInstance()
      try session.setCategory(.playAndRecord, mode: .default, options: [.defaultToSpeaker])
      try session.setActive(true)
      let recorder = try AVAudioRecorder(url: URL(fileURLWithPath: path), settings: [
        AVFormatIDKey: kAudioFormatMPEG4AAC,
        AVSampleRateKey: 44100.0,
        AVNumberOfChannelsKey: 1,
        AVEncoderBitRateKey: 64000,
        AVEncoderAudioQualityKey: AVAudioQuality.high.rawValue,
      ])
      recorder.delegate = self
      guard recorder.prepareToRecord(), recorder.record() else {
        throw AudioFailure.operation
      }
      self.recorder = recorder
      recording_state = "recording"
      last_elapsed_ms = 0
      invoke.resolve(["remember": remember])
    } catch {
      try? FileManager.default.removeItem(atPath: path)
      deactivateAudioSession()
      invoke.reject("Could not start the microphone recording.")
    }
  }

  @objc func pause(_ invoke: Invoke) {
    DispatchQueue.main.async {
      guard self.sessionOpen(invoke) else { return }
      guard self.recording_state == "recording" else { invoke.reject("No recording is running."); return }
      self.recorder?.pause()
      self.recording_state = "paused"
      invoke.resolve()
    }
  }

  @objc func resume(_ invoke: Invoke) {
    DispatchQueue.main.async {
      guard self.sessionOpen(invoke) else { return }
      guard UIApplication.shared.applicationState == .active else {
        invoke.reject("Apell must be in the foreground to record."); return
      }
      guard self.recording_state == "paused", let recorder = self.recorder else {
        invoke.reject("No recording is paused."); return
      }
      do {
        try AVAudioSession.sharedInstance().setActive(true)
        guard recorder.record() else { throw AudioFailure.operation }
        self.recording_state = "recording"
        invoke.resolve()
      } catch { invoke.reject("Could not resume the recording.") }
    }
  }

  @objc func stop(_ invoke: Invoke) {
    DispatchQueue.main.async {
      guard self.sessionOpen(invoke) else { return }
      guard let recorder = self.recorder else { invoke.reject("No recording is active."); return }
      self.last_elapsed_ms = recorder.currentTime * 1000
      recorder.stop()
      self.recorder = nil
      self.recording_state = "idle"
      self.deactivateAudioSession()
      invoke.resolve(["duration_ms": self.last_elapsed_ms])
    }
  }

  @objc func importAudio(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(ImportArgs.self)
    DispatchQueue.main.async {
      guard self.sessionOpen(invoke) else { return }
      guard self.recorder == nil, self.picker == nil else { invoke.reject("Finish the current audio operation first."); return }
      guard let presenter = self.manager.viewController, presenter.presentedViewController == nil else {
        invoke.reject("The audio picker is unavailable."); return
      }
      let picker = AudioPicker(directory: args.directory) { [weak self] result in
        self?.picker = nil
        switch result {
        case .success(let asset): invoke.resolve(asset)
        case .failure: invoke.reject("Could not import audio. Choose a supported audio file smaller than 128 MiB.")
        }
      }
      self.picker = picker
      presenter.present(picker.controller, animated: true)
    }
  }

  @objc func share(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(ShareArgs.self)
    DispatchQueue.main.async {
      guard self.sessionOpen(invoke) else { return }
      guard self.recorder == nil else { invoke.reject("Finish recording before sharing."); return }
      guard let presenter = self.manager.viewController, presenter.presentedViewController == nil else {
        invoke.reject("The share sheet is unavailable."); return
      }
      guard self.pending_share == nil else { invoke.reject("A share sheet is already open."); return }
      self.pending_share = invoke
      let generation = self.generation
      let folder = FileManager.default.temporaryDirectory.appendingPathComponent("apell-audio-share-" + UUID().uuidString)
      self.share_folder = folder
      DispatchQueue.global(qos: .userInitiated).async {
        do {
          try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
          let name = URL(fileURLWithPath: args.file_name).lastPathComponent
          guard !name.isEmpty, name != ".", name != ".." else { throw AudioFailure.operation }
          let file = folder.appendingPathComponent(name)
          try FileManager.default.copyItem(at: URL(fileURLWithPath: args.path), to: file)
          DispatchQueue.main.async {
            guard self.generation == generation, self.pending_share === invoke else {
              try? FileManager.default.removeItem(at: folder)
              return
            }
            let sheet = UIActivityViewController(activityItems: [file], applicationActivities: nil)
            self.share_sheet = sheet
            sheet.completionWithItemsHandler = { [weak self] _, _, _, _ in
              try? FileManager.default.removeItem(at: folder)
              if self?.pending_share === invoke {
                self?.pending_share = nil
                self?.share_sheet = nil
                self?.share_folder = nil
                invoke.resolve()
              }
            }
            sheet.popoverPresentationController?.sourceView = presenter.view
            sheet.popoverPresentationController?.sourceRect = CGRect(x: presenter.view.bounds.midX, y: presenter.view.bounds.midY, width: 1, height: 1)
            presenter.present(sheet, animated: true)
          }
        } catch {
          try? FileManager.default.removeItem(at: folder)
          DispatchQueue.main.async {
            if self.pending_share === invoke {
              self.pending_share = nil
              invoke.reject("Could not prepare the file for sharing.")
            }
          }
        }
      }
    }
  }

  @objc func play(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(PathArgs.self)
    DispatchQueue.main.async {
      guard self.sessionOpen(invoke) else { return }
      guard self.recorder == nil else { invoke.reject("Finish recording before playback."); return }
      do {
        let url = URL(fileURLWithPath: args.path)
        let player: AVAudioPlayer
        if let loaded = self.player, loaded.url == url {
          player = loaded
        } else {
          player = try AVAudioPlayer(contentsOf: url)
          player.delegate = self
        }
        let session = AVAudioSession.sharedInstance()
        try session.setCategory(.playback)
        try session.setActive(true)
        if self.player !== player {
          self.player?.stop()
          self.player = player
        }
        if player.currentTime >= player.duration { player.currentTime = 0 }
        guard player.play() else {
          self.releaseIdlePlaybackSession()
          throw AudioFailure.operation
        }
        invoke.resolve()
      } catch { invoke.reject("This audio file could not be played.") }
    }
  }

  @objc func playbackPause(_ invoke: Invoke) {
    DispatchQueue.main.async {
      guard self.sessionOpen(invoke) else { return }
      if let player = self.player {
        player.pause()
        self.releaseIdlePlaybackSession()
      }
      invoke.resolve()
    }
  }

  @objc func seek(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(SeekArgs.self)
    DispatchQueue.main.async {
      guard self.sessionOpen(invoke) else { return }
      guard let player = self.player, args.position_ms.isFinite else {
        invoke.reject("Load an audio file before seeking."); return
      }
      player.currentTime = min(max(0, args.position_ms / 1000), player.duration)
      invoke.resolve()
    }
  }

  @objc func status(_ invoke: Invoke) {
    DispatchQueue.main.async {
      guard self.sessionOpen(invoke) else { return }
      invoke.resolve([
        "recording_state": self.recording_state,
        "elapsed_ms": self.recorder.map { $0.currentTime * 1000 } ?? self.last_elapsed_ms,
        "playing": self.player?.isPlaying ?? false,
        "position_ms": (self.player?.currentTime ?? 0) * 1000,
        "duration_ms": (self.player?.duration ?? 0) * 1000,
      ] as [String: Any])
    }
  }

  @objc func reset(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(SessionArgs.self)
    DispatchQueue.main.async {
      self.closed_sessions.insert(args.session_id)
      self.generation += 1
      self.cancelPendingStart("The audio session closed.")
      self.pending_share?.resolve()
      self.pending_share = nil
      self.share_sheet?.dismiss(animated: true)
      self.share_sheet = nil
      if let folder = self.share_folder { try? FileManager.default.removeItem(at: folder) }
      self.share_folder = nil
      self.recorder?.stop()
      self.recorder = nil
      self.recording_state = "idle"
      self.last_elapsed_ms = 0
      self.player?.stop()
      self.player = nil
      self.picker?.cancel()
      self.picker = nil
      self.deactivateAudioSession()
      invoke.resolve()
    }
  }

  private func sessionOpen(_ invoke: Invoke) -> Bool {
    guard let args = try? invoke.parseArgs(SessionArgs.self), !args.session_id.isEmpty else {
      invoke.reject("An audio session is required.")
      return false
    }
    guard !closed_sessions.contains(args.session_id) else {
      invoke.reject("The audio session closed.")
      return false
    }
    return true
  }

  func audioRecorderEncodeErrorDidOccur(_ recorder: AVAudioRecorder, error: Error?) {
    recording_state = "paused"
    recorder.pause()
  }

  func audioPlayerDidFinishPlaying(_ player: AVAudioPlayer, successfully flag: Bool) {
    DispatchQueue.main.async {
      guard self.player === player else { return }
      self.releaseIdlePlaybackSession()
    }
  }

  func audioPlayerDecodeErrorDidOccur(_ player: AVAudioPlayer, error: Error?) {
    DispatchQueue.main.async {
      guard self.player === player else { return }
      player.stop()
      self.player = nil
      self.releaseIdlePlaybackSession()
    }
  }

  private func releaseIdlePlaybackSession() {
    guard recorder == nil, player?.isPlaying != true else { return }
    deactivateAudioSession()
  }

  private func deactivateAudioSession() {
    try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
  }
}

enum AudioFailure: Error { case operation }

@_cdecl("init_plugin_apell_audio")
func initPlugin() -> Plugin { AudioPlugin() }
