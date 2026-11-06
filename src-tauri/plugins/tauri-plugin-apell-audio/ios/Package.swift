// swift-tools-version:5.3
import PackageDescription

let package = Package(
  name: "tauri-plugin-apell-audio",
  platforms: [.iOS(.v14)],
  products: [.library(name: "tauri-plugin-apell-audio", type: .static, targets: ["tauri-plugin-apell-audio"])],
  dependencies: [.package(name: "Tauri", path: "../.tauri/tauri-api")],
  targets: [.target(name: "tauri-plugin-apell-audio", dependencies: [.byName(name: "Tauri")], path: "Sources")]
)
