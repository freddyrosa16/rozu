//! Throwaway Tauri 2 spike: Rust backend emits simulated tokens; static HTML renders a streaming list.
use tauri::{Emitter, Manager};
use std::time::Duration;

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let win = app.get_webview_window("main").unwrap();
            std::thread::spawn(move || {
                for i in 1..=2000u32 {
                    std::thread::sleep(Duration::from_millis(16));
                    let _ = win.emit("token", format!("token #{i}: streamed text line"));
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri spike");
}
