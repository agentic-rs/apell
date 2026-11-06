import Foundation
import Tauri
import UIKit

private struct TransferFileArgs: Decodable {
  let path: String
}

/// Export an already encrypted file using the system Files picker.
final class TransferFilePicker: NSObject, UIDocumentPickerDelegate, UIAdaptivePresentationControllerDelegate {
  private var pending: (UIDocumentPickerViewController, Invoke)?

  func save(_ invoke: Invoke, from presenter: UIViewController?) {
    guard pending == nil, let presenter = presenter,
      presenter.viewIfLoaded?.window != nil, presenter.presentedViewController == nil else {
      invoke.reject("Finish the current native dialog first.")
      return
    }
    guard let args = try? invoke.parseArgs(TransferFileArgs.self) else {
      invoke.reject("Could not prepare the settings file.")
      return
    }
    let file = URL(fileURLWithPath: args.path)
    let picker = UIDocumentPickerViewController(forExporting: [file], asCopy: true)
    picker.delegate = self
    pending = (picker, invoke)
    presenter.present(picker, animated: true)
    picker.presentationController?.delegate = self
  }

  func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) {
    finish(controller, saved: !urls.isEmpty)
  }

  func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) {
    finish(controller, saved: false)
  }

  func presentationControllerDidDismiss(_ controller: UIPresentationController) {
    guard let picker = controller.presentedViewController as? UIDocumentPickerViewController else { return }
    finish(picker, saved: false)
  }

  private func finish(_ controller: UIDocumentPickerViewController, saved: Bool) {
    guard let (picker, invoke) = pending, picker === controller else { return }
    pending = nil
    invoke.resolve(["saved": saved])
  }
}
