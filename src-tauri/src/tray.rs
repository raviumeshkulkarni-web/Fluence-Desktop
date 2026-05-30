// Fluence Windows — System Tray module
// Sets up the tray icon with context menu and state indicator.

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager,
};


pub fn setup_tray(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let open_settings = MenuItem::with_id(app, "open_settings", "Open Settings", true, None::<&str>)?;
    let history_item = MenuItem::with_id(app, "history", "Transcription History", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;

    // Recording mode submenu
    let mode_toggle = MenuItem::with_id(app, "mode_toggle", "Push-to-Toggle", true, None::<&str>)?;
    let mode_hold = MenuItem::with_id(app, "mode_hold", "Hold-to-Record", true, None::<&str>)?;
    let mode_submenu = Submenu::with_items(app, "Recording Mode", true, &[&mode_toggle, &mode_hold])?;

    let quit = MenuItem::with_id(app, "quit", "Quit Fluence", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &open_settings,
            &history_item,
            &sep1,
            &mode_submenu,
            &sep2,
            &quit,
        ],
    )?;

    // Load the tray icon explicitly from the embedded 128x128.png file
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/128x128.png"))
        .unwrap_or_else(|e| {
            log::error!("Failed to load tray icon: {}", e);
            app.default_window_icon().cloned().unwrap()
        });

    let tray_builder = TrayIconBuilder::with_id("fluence-tray")
        .tooltip("Fluence — AI Voice Typing")
        .icon(icon);

    let _tray = tray_builder
        .menu(&menu)
        .menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open_settings" => {
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            }
            "history" => {
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.set_focus();
                    // Emit event to navigate to history tab
                    let _ = win.emit("navigate-to", "history");
                }
            }
            "mode_toggle" | "mode_hold" => {
                // Emit event to settings window to update mode
                let new_mode = if event.id.as_ref() == "mode_toggle" {
                    "push_to_toggle"
                } else {
                    "hold_to_record"
                };
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.emit("set-recording-mode", new_mode);
                }
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;

    Ok(())
}
