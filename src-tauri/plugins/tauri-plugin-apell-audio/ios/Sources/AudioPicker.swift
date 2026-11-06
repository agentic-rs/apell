import AVFoundation
import Foundation
import UIKit
import UniformTypeIdentifiers

struct ImportedAudio: Encodable {
  let path: String
  let file_name: String
  let mime_type: String
  let duration_ms: Double
  let size_bytes: Int
}

/// Retained by the plugin until the picker completes. Copy on a worker queue so
/// large or cloud-backed documents do not block the app's interface.
final class AudioPicker: NSObject, UIDocumentPickerDelegate {
  let controller = UIDocumentPickerViewController(forOpeningContentTypes: [.audio], asCopy: true)
  private let copy_progress = Progress(totalUnitCount: 2 * 1024 * 1024 * 1024)
  private let directory: String
  private var completion: ((Result<ImportedAudio?, Error>) -> Void)?

  init(directory: String, completion: @escaping (Result<ImportedAudio?, Error>) -> Void) {
    self.directory = directory
    self.completion = completion
    super.init()
    controller.delegate = self
    controller.allowsMultipleSelection = false
  }

  func cancel() {
    copy_progress.cancel()
    controller.dismiss(animated: true)
    finish(.success(nil))
  }

  func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) { finish(.success(nil)) }

  func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) {
    guard let source = urls.first else { finish(.success(nil)); return }
    DispatchQueue.global(qos: .userInitiated).async {
      let scoped = source.startAccessingSecurityScopedResource()
      defer { if scoped { source.stopAccessingSecurityScopedResource() } }
      let ext = source.pathExtension.lowercased()
      let safe_ext = ext.range(of: "^[a-z0-9]{1,8}$", options: .regularExpression) != nil ? ext : "audio"
      let destination = URL(fileURLWithPath: self.directory).appendingPathComponent(UUID().uuidString + "." + safe_ext)
      do {
        let size = try Self.copyBounded(source, destination, progress: self.copy_progress)
        let audio = AVURLAsset(url: destination)
        let duration = CMTimeGetSeconds(audio.duration)
        guard !audio.tracks(withMediaType: .audio).isEmpty, duration.isFinite, duration > 0,
          duration <= 3 * 60 * 60 else { throw AudioFailure.operation }
        let asset = ImportedAudio(path: destination.path, file_name: source.lastPathComponent,
          mime_type: UTType(filenameExtension: ext)?.preferredMIMEType ?? "application/octet-stream",
          duration_ms: duration * 1000, size_bytes: size)
        DispatchQueue.main.async {
          // A reset can dismiss the picker while its cloud copy is in flight.
          if self.completion == nil { try? FileManager.default.removeItem(at: destination) }
          self.finish(.success(asset))
        }
      } catch {
        try? FileManager.default.removeItem(at: destination)
        DispatchQueue.main.async { self.finish(.failure(AudioFailure.operation)) }
      }
    }
  }

  private func finish(_ result: Result<ImportedAudio?, Error>) {
    let callback = completion
    completion = nil
    callback?(result)
  }

  private static func copyBounded(_ source: URL, _ destination: URL, progress: Progress) throws -> Int {
    guard let input = InputStream(url: source), let output = OutputStream(url: destination, append: false) else {
      throw AudioFailure.operation
    }
    input.open()
    output.open()
    defer { input.close(); output.close() }
    var buffer = [UInt8](repeating: 0, count: 64 * 1024)
    var total = 0
    while true {
      guard !progress.isCancelled else { throw AudioFailure.operation }
      let count = input.read(&buffer, maxLength: buffer.count)
      if count == 0 { break }
      guard count > 0, total + count <= 2 * 1024 * 1024 * 1024 else { throw AudioFailure.operation }
      try buffer.withUnsafeBufferPointer { bytes in
        var written = 0
        while written < count {
          let next = output.write(bytes.baseAddress!.advanced(by: written), maxLength: count - written)
          guard next > 0 else { throw AudioFailure.operation }
          written += next
        }
      }
      total += count
    }
    guard total > 0 else { throw AudioFailure.operation }
    return total
  }
}
