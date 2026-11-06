// swift-tools-version:5.3
import PackageDescription

let package = Package(
  name: "tauri-plugin-apell-sensors",
  platforms: [.iOS(.v14)],
  products: [
    .library(name: "tauri-plugin-apell-sensors", type: .static, targets: ["tauri-plugin-apell-sensors"]),
  ],
  dependencies: [.package(name: "Tauri", path: "../.tauri/tauri-api")],
  targets: [
    .target(name: "tauri-plugin-apell-sensors", dependencies: [.byName(name: "Tauri")], path: "Sources"),
  ]
)
