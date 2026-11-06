import AVFoundation
import CoreImage
import UIKit

enum TransferQrPayload {
  static func isValid(_ payload: String) -> Bool {
    let prefix = "apell-settings:v1:"
    return payload.hasPrefix(prefix)
      && payload.utf8.count > prefix.utf8.count
      && payload.utf8.count <= 1_200
      && payload.utf8.allSatisfy { (33...126).contains($0) }
  }

  static func image(_ payload: String) -> UIImage? {
    guard let filter = CIFilter(name: "CIQRCodeGenerator") else { return nil }
    filter.setValue(Data(payload.utf8), forKey: "inputMessage")
    filter.setValue("M", forKey: "inputCorrectionLevel")
    guard let code = filter.outputImage,
      let image = CIContext().createCGImage(code, from: code.extent) else { return nil }
    return UIImage(cgImage: image)
  }
}

/// A native view keeps the code out of the WebView and can remove it
/// synchronously before UIKit takes an app switcher snapshot.
final class TransferQrDisplay: UIViewController {
  private var qrImage: UIImage?
  private var onClose: (() -> Void)?
  private var observers: [NSObjectProtocol] = []
  private let stack = UIStackView()
  private let codeBackground = UIView()
  private let codeView = UIImageView()
  private let revealButton = UIButton(type: .system)
  private let status = UILabel()

  init(image: UIImage, onClose: @escaping () -> Void) {
    self.qrImage = image
    self.onClose = onClose
    super.init(nibName: nil, bundle: nil)
    modalPresentationStyle = .fullScreen
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) { fatalError("Use init(image:onClose:)") }

  override func viewDidLoad() {
    super.viewDidLoad()
    view.backgroundColor = .systemBackground

    let title = transferLabel("Transfer settings", style: .title2)
    let notice = transferLabel(
      "iOS can still take screenshots. This code is encrypted. Share the passphrase separately.",
      style: .body
    )
    revealButton.setTitle("Reveal QR code", for: .normal)
    revealButton.addTarget(self, action: #selector(reveal), for: .touchUpInside)
    let close = UIButton(type: .system)
    close.setTitle("Done", for: .normal)
    close.addTarget(self, action: #selector(finish), for: .touchUpInside)

    codeView.contentMode = .scaleAspectFit
    codeView.layer.magnificationFilter = .nearest
    codeView.layer.minificationFilter = .nearest
    codeView.accessibilityLabel = "Encrypted settings QR code"
    codeView.isAccessibilityElement = true
    codeBackground.backgroundColor = .white
    codeBackground.addSubview(codeView)
    codeView.translatesAutoresizingMaskIntoConstraints = false
    NSLayoutConstraint.activate([
      codeView.leadingAnchor.constraint(equalTo: codeBackground.leadingAnchor, constant: 16),
      codeView.trailingAnchor.constraint(equalTo: codeBackground.trailingAnchor, constant: -16),
      codeView.topAnchor.constraint(equalTo: codeBackground.topAnchor, constant: 16),
      codeView.bottomAnchor.constraint(equalTo: codeBackground.bottomAnchor, constant: -16),
      codeBackground.heightAnchor.constraint(equalTo: codeBackground.widthAnchor),
    ])

    status.textAlignment = .center
    status.textColor = .secondaryLabel
    status.font = .preferredFont(forTextStyle: .footnote)
    status.numberOfLines = 0
    status.isHidden = true

    for item in [title, notice, status, revealButton, close] { stack.addArrangedSubview(item) }
    stack.axis = .vertical
    stack.spacing = 16
    transferInstall(stack, in: view)

    let center = NotificationCenter.default
    observers = [
      center.addObserver(forName: UIApplication.willResignActiveNotification, object: nil, queue: .main) {
        [weak self] _ in self?.finish()
      },
      center.addObserver(forName: UIScene.willDeactivateNotification, object: nil, queue: .main) {
        [weak self] notice in
        guard let self = self, notice.object as? UIWindowScene === self.view.window?.windowScene else { return }
        self.finish()
      },
      center.addObserver(forName: UIApplication.userDidTakeScreenshotNotification, object: nil, queue: .main) {
        [weak self] _ in if self?.codeView.image != nil { self?.finish() }
      },
    ]
    if #available(iOS 17.0, *) {
      registerForTraitChanges([UITraitSceneCaptureState.self]) { (controller: TransferQrDisplay, _) in
        controller.updateCapture()
      }
    } else {
      observers.append(center.addObserver(forName: UIScreen.capturedDidChangeNotification, object: nil, queue: .main) {
        [weak self] _ in self?.updateCapture()
      })
    }
    updateCapture()
  }

  override func viewDidAppear(_ animated: Bool) {
    super.viewDidAppear(animated)
    updateCapture()
  }

  private var screenCaptured: Bool {
    if #available(iOS 17.0, *) {
      return traitCollection.sceneCaptureState != .inactive
    }
    return view.window?.screen.isCaptured ?? true
  }

  private func updateCapture() {
    if screenCaptured && codeView.image != nil { finish(); return }
    revealButton.isEnabled = !screenCaptured && view.window != nil
    status.text = "Stop screen sharing or recording to reveal the code."
    status.isHidden = !screenCaptured || view.window == nil
  }

  @objc private func reveal() {
    guard UIApplication.shared.applicationState == .active, !screenCaptured, let qrImage = qrImage else {
      updateCapture()
      return
    }
    codeView.image = qrImage
    stack.insertArrangedSubview(codeBackground, at: stack.arrangedSubviews.count - 1)
    revealButton.isHidden = true
  }

  @objc private func finish() {
    guard let complete = onClose else { return }
    onClose = nil
    codeView.image = nil
    qrImage = nil
    codeBackground.removeFromSuperview()
    observers.forEach(NotificationCenter.default.removeObserver)
    observers.removeAll()
    if presentingViewController == nil { complete() } else {
      dismiss(animated: false, completion: complete)
    }
  }

  deinit { observers.forEach(NotificationCenter.default.removeObserver) }
}

final class TransferQrScanner: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
  private let session = AVCaptureSession()
  private let captureQueue = DispatchQueue(label: "dev.apell.transfer-qr-camera")
  private let preview = UIView()
  private let status = UILabel()
  private var previewLayer: AVCaptureVideoPreviewLayer?
  private var onResult: ((Result<String?, TransferQrScanFailure>) -> Void)?
  private var observers: [NSObjectProtocol] = []
  private var started = false

  init(onResult: @escaping (Result<String?, TransferQrScanFailure>) -> Void) {
    self.onResult = onResult
    super.init(nibName: nil, bundle: nil)
    modalPresentationStyle = .fullScreen
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) { fatalError("Use init(onResult:)") }

  override func viewDidLoad() {
    super.viewDidLoad()
    view.backgroundColor = .systemBackground
    let title = transferLabel("Scan settings QR code", style: .title2)
    status.text = "Point the camera at the other device’s Apell QR code."
    status.textAlignment = .center
    status.font = .preferredFont(forTextStyle: .body)
    status.numberOfLines = 0
    preview.backgroundColor = .black
    preview.clipsToBounds = true
    let close = UIButton(type: .system)
    close.setTitle("Cancel", for: .normal)
    close.addTarget(self, action: #selector(cancel), for: .touchUpInside)
    let stack = UIStackView(arrangedSubviews: [title, status, preview, close])
    stack.axis = .vertical
    stack.spacing = 16
    transferInstall(stack, in: view)
    preview.heightAnchor.constraint(equalTo: preview.widthAnchor, multiplier: 0.75).isActive = true

    let layer = AVCaptureVideoPreviewLayer(session: session)
    layer.videoGravity = .resizeAspectFill
    preview.layer.addSublayer(layer)
    previewLayer = layer
    let center = NotificationCenter.default
    observers = [
      center.addObserver(forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: .main) {
        [weak self] _ in self?.cancel()
      },
      center.addObserver(forName: UIScene.didEnterBackgroundNotification, object: nil, queue: .main) {
        [weak self] notice in
        guard let self = self, notice.object as? UIWindowScene === self.view.window?.windowScene else { return }
        self.cancel()
      },
    ]
  }

  override func viewDidLayoutSubviews() {
    super.viewDidLayoutSubviews()
    previewLayer?.frame = preview.bounds
    if let connection = previewLayer?.connection, connection.isVideoOrientationSupported {
      switch view.window?.windowScene?.interfaceOrientation {
      case .landscapeLeft: connection.videoOrientation = .landscapeLeft
      case .landscapeRight: connection.videoOrientation = .landscapeRight
      case .portraitUpsideDown: connection.videoOrientation = .portraitUpsideDown
      default: connection.videoOrientation = .portrait
      }
    }
  }

  override func viewDidAppear(_ animated: Bool) {
    super.viewDidAppear(animated)
    guard !started else { return }
    started = true
    switch AVCaptureDevice.authorizationStatus(for: .video) {
    case .authorized: beginCapture()
    case .notDetermined:
      AVCaptureDevice.requestAccess(for: .video) { [weak self] allowed in
        DispatchQueue.main.async {
          if allowed { self?.beginCapture() } else { self?.finish(.failure(.permission)) }
        }
      }
    default: finish(.failure(.permission))
    }
  }

  private func beginCapture() {
    guard onResult != nil, UIApplication.shared.applicationState != .background else { return }
    captureQueue.async { [weak self] in
      guard let self = self else { return }
      if self.configure() {
        self.session.startRunning()
        if !self.session.isRunning {
          DispatchQueue.main.async { self.finish(.failure(.camera)) }
        }
      } else {
        DispatchQueue.main.async { self.finish(.failure(.camera)) }
      }
    }
  }

  private func configure() -> Bool {
    session.beginConfiguration()
    defer { session.commitConfiguration() }
    session.sessionPreset = .high
    guard let camera = AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: .back),
      let input = try? AVCaptureDeviceInput(device: camera),
      session.canAddInput(input) else { return false }
    session.addInput(input)
    let metadata = AVCaptureMetadataOutput()
    guard session.canAddOutput(metadata) else { return false }
    session.addOutput(metadata)
    guard metadata.availableMetadataObjectTypes.contains(.qr) else { return false }
    metadata.setMetadataObjectsDelegate(self, queue: .main)
    metadata.metadataObjectTypes = [.qr]
    return true
  }

  func metadataOutput(
    _ output: AVCaptureMetadataOutput,
    didOutput objects: [AVMetadataObject],
    from connection: AVCaptureConnection
  ) {
    guard onResult != nil, UIApplication.shared.applicationState == .active else { return }
    let codes = objects.compactMap { ($0 as? AVMetadataMachineReadableCodeObject)?.stringValue }
    if let payload = codes.first(where: TransferQrPayload.isValid) {
      finish(.success(payload))
    } else if !codes.isEmpty {
      status.text = "That QR code isn’t an Apell settings transfer."
    }
  }

  @objc private func cancel() { finish(.success(nil)) }

  private func finish(_ result: Result<String?, TransferQrScanFailure>) {
    guard let complete = onResult else { return }
    onResult = nil
    observers.forEach(NotificationCenter.default.removeObserver)
    observers.removeAll()
    previewLayer?.removeFromSuperlayer()
    let capture = session
    captureQueue.async { [weak self] in
      if capture.isRunning { capture.stopRunning() }
      DispatchQueue.main.async {
        guard let self = self, self.presentingViewController != nil else { complete(result); return }
        self.dismiss(animated: false) { complete(result) }
      }
    }
  }

  deinit { observers.forEach(NotificationCenter.default.removeObserver) }
}

enum TransferQrScanFailure: Error {
  case permission, camera

  var message: String {
    switch self {
    case .permission: return "Allow Apell to use the camera in iOS Settings to scan a QR code."
    case .camera: return "The camera is unavailable for QR scanning."
    }
  }
}

private func transferLabel(_ text: String, style: UIFont.TextStyle) -> UILabel {
  let label = UILabel()
  label.text = text
  label.font = .preferredFont(forTextStyle: style)
  label.adjustsFontForContentSizeCategory = true
  label.textAlignment = .center
  label.numberOfLines = 0
  return label
}

private func transferInstall(_ stack: UIStackView, in view: UIView) {
  let scroll = UIScrollView()
  scroll.alwaysBounceVertical = false
  view.addSubview(scroll)
  scroll.addSubview(stack)
  scroll.translatesAutoresizingMaskIntoConstraints = false
  stack.translatesAutoresizingMaskIntoConstraints = false
  let safe = view.safeAreaLayoutGuide
  let content = scroll.contentLayoutGuide
  let frame = scroll.frameLayoutGuide
  let width = stack.widthAnchor.constraint(equalTo: frame.widthAnchor, constant: -48)
  width.priority = .defaultHigh
  NSLayoutConstraint.activate([
    scroll.leadingAnchor.constraint(equalTo: safe.leadingAnchor),
    scroll.trailingAnchor.constraint(equalTo: safe.trailingAnchor),
    scroll.topAnchor.constraint(equalTo: safe.topAnchor),
    scroll.bottomAnchor.constraint(equalTo: safe.bottomAnchor),
    content.widthAnchor.constraint(equalTo: frame.widthAnchor),
    stack.topAnchor.constraint(equalTo: content.topAnchor, constant: 24),
    stack.bottomAnchor.constraint(equalTo: content.bottomAnchor, constant: -24),
    stack.centerXAnchor.constraint(equalTo: frame.centerXAnchor),
    stack.widthAnchor.constraint(lessThanOrEqualTo: frame.widthAnchor, constant: -48),
    stack.widthAnchor.constraint(lessThanOrEqualToConstant: 480),
    width,
  ])
}
