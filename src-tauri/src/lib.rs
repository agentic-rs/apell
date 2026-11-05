mod applets;
mod assets;
mod backend_services;
mod commands;
mod host;
mod install;
#[cfg(target_os = "ios")]
mod ios;
mod navigation;
mod provider_commands;
mod settings;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
  tauri::Builder::default()
    .register_uri_scheme_protocol("apel", |context, request| {
      assets::respond(context.app_handle(), request)
    })
    .plugin(tauri_plugin_apell_credentials::init())
    .plugin(
      tauri_plugin_opener::Builder::new()
        .open_js_links_on_click(false)
        .build(),
    )
    .setup(|app| {
      let root = app.path().app_data_dir()?;
      let host = host::Host::new(root).map_err(std::io::Error::other)?;
      let origins = navigation::trusted_origins(app.config().build.dev_url.as_ref());
      let boot = navigation::bootstrap(&host.host_key, &origins)?;
      app.manage(host);
      let window =
        tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::App("index.html".into()))
          .title("Apell")
          .background_color(tauri::window::Color(247, 248, 243, 255))
          .initialization_script(boot)
          .on_navigation(move |url| navigation::allowed(url, &origins));
      // On iOS an explicit inner size becomes the actual native view frame.
      // Mobile must inherit the device bounds for responsive CSS and safe areas.
      #[cfg(desktop)]
      let window = window.inner_size(1100., 800.).min_inner_size(320., 480.);
      let _window = window.build()?;
      #[cfg(target_os = "ios")]
      ios::configure_safe_area(&_window)?;
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
    .run(tauri::generate_context!())
    .expect("Could not start Apell");
}
