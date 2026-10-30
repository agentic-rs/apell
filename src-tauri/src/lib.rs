mod commands;
mod host;
mod navigation;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
  tauri::Builder::default()
    .plugin(tauri_plugin_apell_credentials::init())
    .setup(|app| {
      let root = app.path().app_data_dir()?;
      let host = host::Host::new(root).map_err(std::io::Error::other)?;
      let origins = navigation::trusted_origins(app.config().build.dev_url.as_ref());
      let boot = navigation::bootstrap(&host.host_key, &origins)?;
      app.manage(host);
      tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::App("index.html".into()))
        .title("Apell")
        .inner_size(1100., 800.)
        .min_inner_size(360., 560.)
        .initialization_script(boot)
        .on_navigation(move |url| navigation::allowed(url, &origins))
        .build()?;
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
