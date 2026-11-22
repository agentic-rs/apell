mod account;
mod applets;
mod assets;
mod backend_services;
mod commands;
mod config_sync;
#[cfg(target_os = "macos")]
mod desktop;
#[cfg(any(test, all(target_os = "ios", debug_assertions)))]
mod dev_install;
mod host;
mod install;
#[cfg(target_os = "ios")]
mod ios;
mod media;
mod navigation;
mod permissions;
mod provider_commands;
mod sensors;
mod settings;
mod settings_transfer;
mod speech;
mod store_commands;
mod transfer_ui;

use tauri::{
  webview::{PermissionKind, PermissionResponse},
  Manager,
};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
  #[cfg(target_os = "ios")]
  let ios_back = ios::BackSignal::default();
  #[cfg(target_os = "ios")]
  let ios_back_setup = ios_back.clone();
  let builder = tauri::Builder::default();
  #[cfg(any(target_os = "ios", target_os = "android", target_os = "macos"))]
  let builder = builder.plugin(tauri_plugin_deep_link::init());
  builder
    .register_uri_scheme_protocol("apel", |context, request| {
      assets::respond(context.app_handle(), request)
    })
    .plugin(tauri_plugin_apell_credentials::init())
    .plugin(tauri_plugin_apell_audio::init())
    .plugin(tauri_plugin_apell_sensors::init())
    .plugin(
      tauri_plugin_opener::Builder::new()
        .open_js_links_on_click(false)
        .build(),
    )
    .setup(move |app| {
      let root = app.path().app_data_dir()?;
      let host = host::Host::new(root).map_err(std::io::Error::other)?;
      #[cfg(all(target_os = "ios", debug_assertions))]
      dev_install::start(app, &host);
      let origins = navigation::trusted_origins(app.config().build.dev_url.as_ref());
      let boot = navigation::bootstrap(&host.host_key, &origins)?;
      app.manage(host);
      #[cfg(any(target_os = "android", target_os = "macos"))]
      account::register_callbacks(app);
      let window =
        tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::App("index.html".into()))
          .title("Apell")
          .background_color(tauri::window::Color(248, 247, 250, 255))
          .initialization_script(boot)
          // Sensor access belongs to the native applet services.
          .on_permission_request(|_, kind| match kind {
            PermissionKind::Microphone | PermissionKind::Camera | PermissionKind::Geolocation => {
              PermissionResponse::Deny
            }
            _ => PermissionResponse::Default,
          })
          .on_navigation(move |url| navigation::allowed(url, &origins));
      // On iOS an explicit inner size becomes the actual native view frame.
      // Mobile must inherit the device bounds for responsive CSS and safe areas.
      #[cfg(desktop)]
      let window = window.inner_size(420., 820.).min_inner_size(320., 480.);
      let _window = window.build()?;
      #[cfg(target_os = "macos")]
      desktop::configure_window(&_window);
      #[cfg(target_os = "ios")]
      ios::configure_webview(&_window, ios_back_setup)?;
      let handle = app.handle().clone();
      tauri::async_runtime::spawn(async move {
        loop {
          tokio::time::sleep(std::time::Duration::from_secs(1)).await;
          let handle = handle.clone();
          let _ = tauri::async_runtime::spawn_blocking(move || {
            handle.state::<host::Host>().sqlite.reap_expired()
          })
          .await;
        }
      });
      Ok(())
    })
    .invoke_handler(tauri::generate_handler![
      commands::shell_command,
      commands::applet_call
    ])
    .build(tauri::generate_context!())
    .expect("Could not start Apell")
    .run(|_app, _event| {
      #[cfg(target_os = "macos")]
      desktop::on_event(_app, &_event);
    });
}
