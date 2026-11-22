fn main() {
  if std::env::var("CARGO_CFG_TARGET_VENDOR").as_deref() == Ok("apple") {
    println!("cargo:rerun-if-changed=src/speech/apple.m");
    cc::Build::new()
      .file("src/speech/apple.m")
      // Embed the framework link in the archive consumed by existing Xcode projects.
      .flag("-fmodules")
      .flag("-fobjc-arc")
      .flag("-fblocks")
      .compile("apell_system_speech");
    println!("cargo:rustc-link-lib=framework=Speech");
    println!("cargo:rustc-link-lib=framework=Foundation");
  }
  if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("ios") {
    println!("cargo:rerun-if-changed=src/account/apple.m");
    cc::Build::new()
      .file("src/account/apple.m")
      .flag("-fmodules")
      .flag("-fobjc-arc")
      .flag("-fblocks")
      .compile("apell_account_authentication");
    println!("cargo:rustc-link-lib=framework=AuthenticationServices");
    println!("cargo:rustc-link-lib=framework=UIKit");
  }
  tauri_build::build();
}
