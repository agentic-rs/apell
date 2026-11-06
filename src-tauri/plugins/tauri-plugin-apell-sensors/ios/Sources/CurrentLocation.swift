import CoreLocation
import Foundation

enum LocationFailure: Error {
  case denied, unavailable, timedOut

  var message: String {
    switch self {
    case .denied: return "Location access is denied. Enable it for Apell in Settings."
    case .unavailable: return "Could not get a current location. Try again."
    case .timedOut: return "Timed out waiting for a current location."
    }
  }
}

/// The plugin creates, drives, and cancels this delegate on the main queue.
final class CurrentLocation: NSObject, CLLocationManagerDelegate {
  private let manager = CLLocationManager()
  private var completion: ((Result<CLLocation, LocationFailure>) -> Void)?
  private var timeout: DispatchWorkItem?

  init(completion: @escaping (Result<CLLocation, LocationFailure>) -> Void) {
    self.completion = completion
    super.init()
  }

  func start() {
    manager.delegate = self
    if manager.authorizationStatus == .notDetermined {
      manager.requestWhenInUseAuthorization()
    } else {
      locationManagerDidChangeAuthorization(manager)
    }
  }

  func cancel() {
    completion = nil
    timeout?.cancel()
    timeout = nil
    manager.delegate = nil
    manager.stopUpdatingLocation()
  }

  func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
    guard completion != nil else { return }
    switch manager.authorizationStatus {
    case .authorizedWhenInUse, .authorizedAlways:
      guard timeout == nil else { return }
      manager.desiredAccuracy = manager.accuracyAuthorization == .reducedAccuracy
        ? kCLLocationAccuracyReduced : kCLLocationAccuracyBest
      let deadline = DispatchWorkItem { [weak self] in self?.finish(.failure(.timedOut)) }
      timeout = deadline
      DispatchQueue.main.asyncAfter(deadline: .now() + .seconds(20), execute: deadline)
      manager.requestLocation()
    case .notDetermined:
      break
    default:
      finish(.failure(.denied))
    }
  }

  func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
    let now = Date()
    guard let fix = locations.last, CLLocationCoordinate2DIsValid(fix.coordinate),
      fix.horizontalAccuracy.isFinite, fix.horizontalAccuracy >= 0,
      (-5...30).contains(now.timeIntervalSince(fix.timestamp)) else {
      finish(.failure(.unavailable))
      return
    }
    finish(.success(fix))
  }

  func locationManager(_ manager: CLLocationManager, didFailWithError error: Error) {
    finish(.failure((error as? CLError)?.code == .denied ? .denied : .unavailable))
  }

  private func finish(_ result: Result<CLLocation, LocationFailure>) {
    let callback = completion
    cancel()
    callback?(result)
  }
}
