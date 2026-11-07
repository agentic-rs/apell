import Foundation
import ImageIO
import PhotosUI
import UIKit
import UniformTypeIdentifiers

enum SensorImageResult {
  case stored, cancelled, failed
}

/// A provider may call back even after cancellation. Only a worker that owns
/// this gate may open the staging file, and cancellation waits for that worker.
private final class ImageWriteGate {
  private let lock = NSLock()
  private var writing = false
  private var cancelled = false

  func begin() -> Bool {
    lock.lock()
    defer { lock.unlock() }
    guard !cancelled, !writing else { return false }
    writing = true
    return true
  }

  func cancel() -> Bool {
    lock.lock()
    defer { lock.unlock() }
    cancelled = true
    return !writing
  }

  var isCancelled: Bool {
    lock.lock()
    defer { lock.unlock() }
    return cancelled
  }
}

final class SensorImagePicker: NSObject, UIImagePickerControllerDelegate,
  UINavigationControllerDelegate, PHPickerViewControllerDelegate, UIAdaptivePresentationControllerDelegate
{
  private static let rasterExtensions: Set<String> = [
    "jpg", "jpeg", "png", "gif", "webp", "heic", "heif", "avif",
  ]
  private let camera: Bool
  private let destination: URL
  private let maxBytes: Int
  private let jpeg: Bool
  private let gate = ImageWriteGate()
  private var controller: UIViewController?
  private var providerLoad: Progress?
  private var completion: ((SensorImageResult) -> Void)?

  init(camera: Bool, path: String, maxBytes: Int, jpeg: Bool = false,
    completion: @escaping (SensorImageResult) -> Void) {
    self.camera = camera
    destination = URL(fileURLWithPath: path)
    self.maxBytes = maxBytes
    self.jpeg = jpeg
    self.completion = completion
    super.init()
  }

  func present(from presenter: UIViewController) {
    let picker: UIViewController
    if camera {
      let capture = UIImagePickerController()
      capture.sourceType = .camera
      capture.mediaTypes = [UTType.image.identifier]
      capture.cameraCaptureMode = .photo
      capture.modalPresentationStyle = .fullScreen
      capture.delegate = self
      picker = capture
    } else {
      var config = PHPickerConfiguration()
      config.filter = .images
      config.selectionLimit = 1
      config.preferredAssetRepresentationMode = .current
      let photos = PHPickerViewController(configuration: config)
      photos.delegate = self
      picker = photos
    }
    controller = picker
    presenter.present(picker, animated: true)
    picker.presentationController?.delegate = self
  }

  func cancel() {
    let canFinish = gate.cancel()
    providerLoad?.cancel()
    controller?.dismiss(animated: false)
    controller = nil
    if canFinish { finish(.cancelled) }
  }

  func presentationControllerDidDismiss(_ presentationController: UIPresentationController) {
    guard controller === presentationController.presentedViewController else { return }
    controller = nil
    finish(.cancelled)
  }

  func imagePickerControllerDidCancel(_ picker: UIImagePickerController) {
    dismiss(picker) { self.finish(.cancelled) }
  }

  func imagePickerController(
    _ picker: UIImagePickerController,
    didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]
  ) {
    let image = info[.originalImage] as? UIImage
    dismiss(picker) {
      guard let image = image else { self.finish(.failed); return }
      DispatchQueue.global(qos: .userInitiated).async {
        self.write {
          let data = try Self.cameraJPEG(image)
          guard data.count > 0, data.count <= self.maxBytes, !self.gate.isCancelled else {
            throw ImageWriteFailure.invalid
          }
          try data.write(to: self.destination, options: .withoutOverwriting)
        }
      }
    }
  }

  func picker(_ picker: PHPickerViewController, didFinishPicking results: [PHPickerResult]) {
    dismiss(picker) {
      guard let item = results.first?.itemProvider else { self.finish(.cancelled); return }
      self.loadPhoto(item)
    }
  }

  private func dismiss(_ picker: UIViewController, then action: @escaping () -> Void) {
    guard controller === picker else { return }
    controller = nil
    picker.dismiss(animated: true) {
      guard self.completion != nil else { return }
      action()
    }
  }

  private func loadPhoto(_ provider: NSItemProvider) {
    let imageTypes = provider.registeredTypeIdentifiers.filter {
      UTType($0)?.conforms(to: .image) == true
    }
    // Prefer the provider's concrete raster representation. A generic image
    // representation is still checked by the host after copying.
    let type = imageTypes.first {
      guard let ext = UTType($0)?.preferredFilenameExtension else { return false }
      return Self.rasterExtensions.contains(ext.lowercased())
    } ?? imageTypes.first { $0 == UTType.image.identifier }
    guard let type = type else { finish(.failed); return }
    providerLoad = provider.loadFileRepresentation(forTypeIdentifier: type) { [weak self] url, _ in
      guard let self = self else { return }
      // The provider deletes this temporary file when its callback returns.
      self.write {
        guard let url = url else { throw ImageWriteFailure.invalid }
        if self.jpeg {
          try self.storeJPEG(from: url)
        } else {
          try self.copyPhoto(from: url, to: self.destination)
        }
      }
    }
  }

  private func write(_ operation: () throws -> Void) {
    guard gate.begin() else { return }
    let result: SensorImageResult
    do {
      try operation()
      result = .stored
    } catch {
      try? FileManager.default.removeItem(at: destination)
      result = .failed
    }
    DispatchQueue.main.async {
      if self.gate.isCancelled {
        try? FileManager.default.removeItem(at: self.destination)
        self.finish(.cancelled)
      } else {
        self.finish(result)
      }
    }
  }

  private func finish(_ result: SensorImageResult) {
    let callback = completion
    completion = nil
    providerLoad = nil
    callback?(result)
  }

  private func copyPhoto(from source: URL, to target: URL) throws {
    guard let input = InputStream(url: source),
      let output = OutputStream(url: target, append: false) else {
      throw ImageWriteFailure.invalid
    }
    input.open()
    output.open()
    defer { input.close(); output.close() }
    var buffer = [UInt8](repeating: 0, count: 64 * 1024)
    var total = 0
    while !gate.isCancelled {
      let count = input.read(&buffer, maxLength: buffer.count)
      if count == 0 && total > 0 { return }
      guard count > 0, count <= maxBytes - total else { throw ImageWriteFailure.invalid }
      try buffer.withUnsafeBufferPointer { bytes in
        var written = 0
        while written < count {
          let next = output.write(bytes.baseAddress!.advanced(by: written), maxLength: count - written)
          guard next > 0 else { throw ImageWriteFailure.invalid }
          written += next
        }
      }
      total += count
    }
    throw ImageWriteFailure.invalid
  }

  private func storeJPEG(from source: URL) throws {
    // Bound the provider file before ImageIO opens it. Its original URL is only
    // valid for this callback, and the temporary copy stays outside applet files.
    let staged = destination.appendingPathExtension("source")
    defer { try? FileManager.default.removeItem(at: staged) }
    try copyPhoto(from: source, to: staged)
    guard !gate.isCancelled,
      let imageSource = CGImageSourceCreateWithURL(staged as CFURL,
        [kCGImageSourceShouldCache: false] as CFDictionary) else {
      throw ImageWriteFailure.invalid
    }
    let options: [CFString: Any] = [
      kCGImageSourceCreateThumbnailFromImageAlways: true,
      kCGImageSourceCreateThumbnailWithTransform: true,
      kCGImageSourceThumbnailMaxPixelSize: 4096,
      kCGImageSourceShouldCacheImmediately: true,
    ]
    guard let thumbnail = CGImageSourceCreateThumbnailAtIndex(imageSource, 0,
      options as CFDictionary), !gate.isCancelled else {
      throw ImageWriteFailure.invalid
    }
    let data = try Self.cameraJPEG(UIImage(cgImage: thumbnail))
    guard !data.isEmpty, data.count <= maxBytes, !gate.isCancelled else {
      throw ImageWriteFailure.invalid
    }
    try data.write(to: destination, options: .withoutOverwriting)
  }

  private static func cameraJPEG(_ image: UIImage) throws -> Data {
    let size = image.size
    guard size.width.isFinite, size.height.isFinite, size.width > 0, size.height > 0 else {
      throw ImageWriteFailure.invalid
    }
    // Redrawing applies the captured orientation without copying EXIF metadata.
    let scale = min(1, 4096 / max(size.width, size.height))
    let output = CGSize(width: max(1, (size.width * scale).rounded()),
      height: max(1, (size.height * scale).rounded()))
    let format = UIGraphicsImageRendererFormat()
    format.scale = 1
    format.opaque = true
    return UIGraphicsImageRenderer(size: output, format: format).jpegData(withCompressionQuality: 0.9) { _ in
      image.draw(in: CGRect(origin: .zero, size: output))
    }
  }
}

private enum ImageWriteFailure: Error {
  case invalid
}
