fn main() {
  if std::env::var("CARGO_CFG_TARGET_VENDOR").as_deref() == Ok("apple") {
    println!("cargo:rerun-if-changed=src/speech/apple.m");
    cc::Build::new()
      .file("src/speech/apple.m")
      .flag("-fobjc-arc")
      .flag("-fblocks")
      .compile("apell_system_speech");
    println!("cargo:rustc-link-lib=framework=Speech");
    println!("cargo:rustc-link-lib=framework=Foundation");
  }
  tauri_build::build();
}
