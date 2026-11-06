import AVFoundation
import Foundation
import Tauri
import UIKit

private struct SensorID: Decodable, Equatable {
  let session_id: String
  let operation_id: String
}

private enum SensorKind: String, Decodable {
  case camera, photos, location

  var accessName: String {
    switch self {
    case .camera: return "the camera"
    case .photos: return "the photo library"
    case .location: return "your location"
    }
  }
}

private struct SensorArgs: Decodable {
  let session_id: String
  let operation_id: String
  let kind: SensorKind
  let applet_name: String
  let remembered: Bool
  let staging_path: String?
  let max_image_bytes: UInt64

  var id: SensorID { SensorID(session_id: session_id, operation_id: operation_id) }
}

private struct TransferQrArgs: Decodable {
  let payload: String
}

private final class SensorOperation {
  let args: SensorArgs
  let invoke: Invoke
  var remember = false
  var consent: UIAlertController?
  var imagePicker: SensorImagePicker?
  var location: CurrentLocation?

  init(args: SensorArgs, invoke: Invoke) {
    self.args = args
    self.invoke = invoke
  }
}

final class SensorsPlugin: Plugin {
  // UIKit, Core Location, and all operation state are confined to the main queue.
  private var active: SensorOperation?
  private var cancelledBeforeRun: SensorID?
  private var background: NSObjectProtocol?
  private var transferQr: UIViewController?
  private let transferFile = TransferFilePicker()

  override init() {
    super.init()
    background = NotificationCenter.default.addObserver(
      forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: .main
    ) { [weak self] _ in
      guard let self = self, let operation = self.active else { return }
      self.cancel(operation)
    }
  }

  deinit {
    if let background = background { NotificationCenter.default.removeObserver(background) }
  }

  @objc func run(_ invoke: Invoke) {
    guard let args = try? invoke.parseArgs(SensorArgs.self) else {
      invoke.reject("Invalid sensor request.")
      return
    }
    DispatchQueue.main.async {
      if self.cancelledBeforeRun == args.id {
        self.cancelledBeforeRun = nil
        invoke.resolve(["status": "cancelled"])
        return
      }
      guard self.active == nil else {
        invoke.reject("Finish the current device request first.")
        return
      }
      guard UIApplication.shared.applicationState == .active, let presenter = self.presenter else {
        invoke.reject("Open the applet in the foreground to use this feature.")
        return
      }
      if args.kind == .camera && !UIImagePickerController.isSourceTypeAvailable(.camera) {
        invoke.reject("The camera is unavailable on this device.")
        return
      }
      let operation = SensorOperation(args: args, invoke: invoke)
      self.active = operation
      if args.kind == .photos || args.remembered {
        self.start(operation)
      } else {
        self.ask(operation, from: presenter)
      }
    }
  }

  @objc func cancel(_ invoke: Invoke) {
    guard let id = try? invoke.parseArgs(SensorID.self) else {
      invoke.reject("Invalid sensor cancellation.")
      return
    }
    DispatchQueue.main.async {
      if let operation = self.active, operation.args.id == id {
        self.cancel(operation)
      } else {
        // Rust keeps one request in flight. Cancel can reach the main queue
        // before that request does, and operation identifiers are never reused.
        self.cancelledBeforeRun = id
      }
      invoke.resolve()
    }
  }

  @objc func showTransferQr(_ invoke: Invoke) {
    guard let args = try? invoke.parseArgs(TransferQrArgs.self),
      TransferQrPayload.isValid(args.payload) else {
      invoke.reject("The settings QR code is invalid.")
      return
    }
    DispatchQueue.main.async {
      guard let presenter = self.transferPresenter else {
        invoke.reject("Open Apell in the foreground to show the settings QR code.")
        return
      }
      guard let image = TransferQrPayload.image(args.payload) else {
        invoke.reject("Could not display the settings QR code.")
        return
      }
      let display = TransferQrDisplay(image: image) { [weak self] in
        self?.transferQr = nil
        invoke.resolve()
      }
      self.transferQr = display
      presenter.present(display, animated: true)
    }
  }

  @objc func scanTransferQr(_ invoke: Invoke) {
    DispatchQueue.main.async {
      guard let presenter = self.transferPresenter else {
        invoke.reject("Open Apell in the foreground to scan a settings QR code.")
        return
      }
      let scanner = TransferQrScanner { [weak self] result in
        self?.transferQr = nil
        switch result {
        case .success(let payload):
          invoke.resolve(["payload": payload.map { $0 as Any } ?? NSNull()])
        case .failure(let error):
          invoke.reject(error.message)
        }
      }
      self.transferQr = scanner
      presenter.present(scanner, animated: true)
    }
  }

  @objc func saveTransferFile(_ invoke: Invoke) {
    DispatchQueue.main.async {
      self.transferFile.save(invoke, from: self.presenter)
    }
  }

  private var transferPresenter: UIViewController? {
    guard active == nil, transferQr == nil, UIApplication.shared.applicationState == .active else {
      return nil
    }
    return presenter
  }

  private var presenter: UIViewController? {
    guard let controller = manager.viewController, controller.viewIfLoaded?.window != nil,
      controller.presentedViewController == nil else { return nil }
    return controller
  }

  private func ask(_ operation: SensorOperation, from presenter: UIViewController) {
    let prompt = UIAlertController(
      title: "Allow access to \(operation.args.kind.accessName)?",
      message: "\(operation.args.applet_name) is requesting access.",
      preferredStyle: .alert
    )
    prompt.addAction(UIAlertAction(title: "Cancel", style: .cancel) { [weak self, weak operation] _ in
      guard let operation = operation else { return }
      self?.complete(operation, ["status": "cancelled"])
    })
    for (title, remember) in [("Allow Once", false), ("Always Allow", true)] {
      prompt.addAction(UIAlertAction(title: title, style: .default) { [weak self, weak operation] _ in
        guard let self = self, let operation = operation, self.active === operation else { return }
        operation.remember = remember
        let proceed = { [weak self, weak operation] in
          guard let self = self, let operation = operation, self.active === operation else { return }
          operation.consent = nil
          self.start(operation)
        }
        if let prompt = operation.consent, prompt.presentingViewController != nil {
          prompt.dismiss(animated: true, completion: proceed)
        } else {
          DispatchQueue.main.async(execute: proceed)
        }
      })
    }
    operation.consent = prompt
    presenter.present(prompt, animated: true)
  }

  private func start(_ operation: SensorOperation) {
    guard active === operation else { return }
    guard UIApplication.shared.applicationState != .background else {
      cancel(operation)
      return
    }
    switch operation.args.kind {
    case .photos: openImagePicker(operation)
    case .camera:
      switch AVCaptureDevice.authorizationStatus(for: .video) {
      case .authorized:
        openImagePicker(operation)
      case .notDetermined:
        AVCaptureDevice.requestAccess(for: .video) { [weak self, weak operation] allowed in
          DispatchQueue.main.async {
            guard let self = self, let operation = operation, self.active === operation else { return }
            if allowed {
              self.openImagePicker(operation)
            } else {
              self.fail(operation, "Camera access is denied. Enable it for Apell in Settings.")
            }
          }
        }
      default:
        fail(operation, "Camera access is denied. Enable it for Apell in Settings.")
      }
    case .location:
      let location = CurrentLocation { [weak self, weak operation] result in
        guard let self = self, let operation = operation else { return }
        switch result {
        case .success(let fix):
          let milliseconds = (fix.timestamp.timeIntervalSince1970 * 1000).rounded(.down)
          guard milliseconds >= 1, milliseconds <= 9_007_199_254_740_991 else {
            self.fail(operation, LocationFailure.unavailable.message)
            return
          }
          self.complete(operation, [
            "status": "location",
            "remember": operation.remember,
            "latitude": fix.coordinate.latitude,
            "longitude": fix.coordinate.longitude,
            "accuracy_m": fix.horizontalAccuracy,
            "timestamp_ms": UInt64(milliseconds),
          ])
        case .failure(let error): self.fail(operation, error.message)
        }
      }
      operation.location = location
      location.start()
    }
  }

  private func openImagePicker(_ operation: SensorOperation) {
    guard active === operation else { return }
    guard UIApplication.shared.applicationState != .background else {
      cancel(operation)
      return
    }
    guard let presenter = presenter else {
      fail(operation, "The image picker is unavailable.")
      return
    }
    guard let path = operation.args.staging_path, !path.isEmpty,
      let limit = Int(exactly: operation.args.max_image_bytes), limit > 0 else {
      fail(operation, "Image storage is unavailable.")
      return
    }
    let camera = operation.args.kind == .camera
    guard !camera || UIImagePickerController.isSourceTypeAvailable(.camera) else {
      fail(operation, "The camera is unavailable on this device.")
      return
    }
    let picker = SensorImagePicker(camera: camera, path: path, maxBytes: limit) {
      [weak self, weak operation] result in
      guard let self = self, let operation = operation else { return }
      switch result {
      case .stored:
        self.complete(operation, ["status": "image", "remember": operation.remember])
      case .cancelled:
        self.complete(operation, ["status": "cancelled"])
      case .failed:
        self.fail(operation, camera
          ? "Could not save the captured photo."
          : "Could not import the selected photo. Choose a supported image within the size limit.")
      }
    }
    operation.imagePicker = picker
    picker.present(from: presenter)
  }

  private func cancel(_ operation: SensorOperation) {
    guard active === operation else { return }
    operation.consent?.dismiss(animated: false)
    operation.consent = nil
    operation.location?.cancel()
    if let picker = operation.imagePicker {
      // The picker settles after a pending image write has cleaned up.
      picker.cancel()
    } else {
      complete(operation, ["status": "cancelled"])
    }
  }

  private func complete(_ operation: SensorOperation, _ result: JsonObject) {
    guard active === operation else { return }
    active = nil
    operation.invoke.resolve(result)
  }

  private func fail(_ operation: SensorOperation, _ message: String) {
    guard active === operation else { return }
    active = nil
    operation.invoke.reject(message)
  }
}

@_cdecl("init_plugin_apell_sensors")
func initSensorsPlugin() -> Plugin { SensorsPlugin() }
