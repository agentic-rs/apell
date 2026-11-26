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
mod key_sync;
#[cfg(all(feature = "local-dev", target_os = "macos", debug_assertions))]
mod local_dev;
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
  let context = tauri::generate_context!();
  #[cfg(all(feature = "local-dev", target_os = "macos", debug_assertions))]
  let (context, local_device) =
    local_dev::device_context(context).expect("Could not configure the local device");
  #[cfg(target_os = "ios")]
  let ios_back = ios::BackSignal::default();
  #[cfg(target_os = "ios")]
  let ios_back_setup = ios_back.clone();
  tauri::Builder::default()
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
      let title = app
        .config()
        .product_name
        .as_deref()
        .unwrap_or(env!("CARGO_PKG_NAME"));
      let window =
        tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::App("index.html".into()))
          .title(title)
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
      #[cfg(all(feature = "local-dev", target_os = "macos", debug_assertions))]
      let window = {
        app.manage(local_device);
        window.incognito(true)
      };
      // On iOS an explicit inner size becomes the actual native view frame.
      // Mobile must inherit the device bounds for responsive CSS and safe areas.
      #[cfg(desktop)]
      let window = window.inner_size(420., 820.).min_inner_size(320., 480.);
      let _window = window.build()?;
      #[cfg(target_os = "macos")]
      desktop::configure_window(&_window);
      #[cfg(target_os = "ios")]
      ios::configure_webview(&_window, ios_back_setup)?;
      key_sync::start(app.handle());
      config_sync::start(app.handle());
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
    .on_window_event(|window, event| {
      if window.label() == "main" && matches!(event, tauri::WindowEvent::Focused(true)) {
        key_sync::kick(window.app_handle());
        config_sync::kick(window.app_handle());
        permissions::publish_device_change(window.app_handle());
      }
    })
    .invoke_handler(tauri::generate_handler![
      commands::shell_command,
      commands::applet_call
    ])
    .build(context)
    .expect("Could not start Apell")
    .run(|_app, _event| {
      if matches!(_event, tauri::RunEvent::Resumed) {
        key_sync::kick(_app);
        config_sync::kick(_app);
        permissions::publish_device_change(_app);
      }
      #[cfg(target_os = "macos")]
      desktop::on_event(_app, &_event);
    });
}
