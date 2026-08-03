// Fluence Windows — System Tray module
// Sets up the tray icon with context menu and state indicator.

use crate::{audio, settings};
use std::sync::{Mutex, OnceLock};
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager,
};

// Keep the tray icon alive for the app's lifetime so the context menu stays live.
static TRAY: OnceLock<Mutex<Option<tauri::tray::TrayIcon>>> = OnceLock::new();

pub fn setup_tray(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    // Load the tray icon explicitly from the embedded 128x128.png file
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/128x128.png"))
        .unwrap_or_else(|e| {
            log::error!("Failed to load tray icon: {}", e);
            app.default_window_icon().cloned().unwrap()
        });

    let tray = TrayIconBuilder::with_id("fluence-tray")
        .tooltip("Fluence — AI Voice Typing")
        .icon(icon)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open_settings" => {
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.set_focus();
                    // Notify main window frontend to resume canvas animations
                    let _ = win.emit("window-visibility", true);
                }
            }
            "history" => {
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.set_focus();
                    // Notify main window frontend to resume canvas animations
                    let _ = win.emit("window-visibility", true);
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

    let _ = TRAY.set(Mutex::new(Some(tray)));

    refresh_menu(app)?;

    Ok(())
}

/// Rebuild the tray context menu from the current live state (recording flag + settings).
pub fn refresh_menu(app: &AppHandle) -> Result<(), String> {
    let Some(tray) = app.tray_by_id("fluence-tray") else {
        log::warn!("Tray icon not found; skipping menu refresh");
        return Ok(());
    };

    let recording = audio::is_recording();

    let status = if recording {
        Some(
            MenuItem::with_id(
                app,
                "recording_status",
                "Recording in progress",
                false,
                None::<&str>,
            )
            .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };

    let open_settings =
        MenuItem::with_id(app, "open_settings", "Open Settings", true, None::<&str>)
            .map_err(|e| e.to_string())?;
    let history_item =
        MenuItem::with_id(app, "history", "Transcription History", true, None::<&str>)
            .map_err(|e| e.to_string())?;
    let sep1 = PredefinedMenuItem::separator(app).map_err(|e| e.to_string())?;
    let sep2 = PredefinedMenuItem::separator(app).map_err(|e| e.to_string())?;

    // Recording mode submenu, checked against the persisted setting
    let current = settings::load_settings().unwrap_or_default();
    let mode_toggle = CheckMenuItem::with_id(
        app,
        "mode_toggle",
        "Push-to-Toggle",
        true,
        current.recording_mode == "push_to_toggle",
        None::<&str>,
    )
    .map_err(|e| e.to_string())?;
    let mode_hold = CheckMenuItem::with_id(
        app,
        "mode_hold",
        "Hold-to-Record",
        true,
        current.recording_mode == "hold_to_record",
        None::<&str>,
    )
    .map_err(|e| e.to_string())?;
    let mode_submenu =
        Submenu::with_items(app, "Recording Mode", true, &[&mode_toggle, &mode_hold])
            .map_err(|e| e.to_string())?;

    let quit = MenuItem::with_id(app, "quit", "Quit Fluence", true, None::<&str>)
        .map_err(|e| e.to_string())?;

    let menu = if recording {
        Menu::with_items(
            app,
            &[
                status.as_ref().unwrap(),
                &open_settings,
                &history_item,
                &sep1,
                &mode_submenu,
                &sep2,
                &quit,
            ],
        )
        .map_err(|e| e.to_string())?
    } else {
        Menu::with_items(
            app,
            &[
                &open_settings,
                &history_item,
                &sep1,
                &mode_submenu,
                &sep2,
                &quit,
            ],
        )
        .map_err(|e| e.to_string())?
    };

    tray.set_menu(Some(menu)).map_err(|e| e.to_string())?;
    Ok(())
}
