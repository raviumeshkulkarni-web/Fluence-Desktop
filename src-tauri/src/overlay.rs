// Fluence Windows — Overlay window management
// Controls the floating, always-on-top recording overlay window.

use tauri::{AppHandle, Emitter, Manager, WebviewWindow};

pub fn get_overlay_window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window("overlay")
}

/// Show the overlay at the configured screen position
#[tauri::command]
pub fn show_overlay(app: AppHandle, position: String) -> Result<(), String> {
    let win = get_overlay_window(&app).ok_or("Overlay window not found")?;

    // Get screen dimensions
    let monitor = win
        .current_monitor()
        .map_err(|e| e.to_string())?
        .or_else(|| win.primary_monitor().ok().flatten())
        .ok_or("No monitor found")?;

    log::debug!(
        "Showing overlay at position: {} on monitor: {:?}",
        position,
        monitor.name()
    );

    let screen_size = monitor.size();
    let scale = monitor.scale_factor();

    let win_width = 260.0;
    let win_height = 146.0;
    let margin = 20.0;

    let (x, y) = match position.as_str() {
        "bottom_left" => (
            margin,
            screen_size.height as f64 / scale - win_height - margin - 24.0,
        ),
        "center" => (
            screen_size.width as f64 / scale / 2.0 - win_width / 2.0,
            screen_size.height as f64 / scale - win_height - margin - 24.0,
        ),
        _ => {
            // bottom_right (default)
            (
                screen_size.width as f64 / scale - win_width - margin,
                screen_size.height as f64 / scale - win_height - margin - 24.0,
            )
        }
    };

    win.set_position(tauri::LogicalPosition::new(x, y))
        .map_err(|e| e.to_string())?;

    win.set_always_on_top(true).map_err(|e| e.to_string())?;
    win.show().map_err(|e| e.to_string())?;
    // Notify overlay frontend to start the waveform animation loop
    let _ = win.emit("window-visibility", true);
    Ok(())
}

#[tauri::command]
pub fn hide_overlay(app: AppHandle) -> Result<(), String> {
    let win = get_overlay_window(&app).ok_or("Overlay window not found")?;
    // Notify overlay frontend to stop the waveform animation loop
    let _ = win.emit("window-visibility", false);
    win.hide().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn show_main_window(app: AppHandle) -> Result<(), String> {
    let win = app
        .get_webview_window("main")
        .ok_or("Main window not found")?;
    win.show().map_err(|e| e.to_string())?;
    win.set_focus().map_err(|e| e.to_string())?;
    // Notify main window frontend to resume canvas animations
    let _ = win.emit("window-visibility", true);
    Ok(())
}

#[tauri::command]
pub fn hide_main_window(app: AppHandle) -> Result<(), String> {
    let win = app
        .get_webview_window("main")
        .ok_or("Main window not found")?;
    // Notify main window frontend to pause canvas animations
    let _ = win.emit("window-visibility", false);
    win.hide().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn minimize_main_window(app: AppHandle) -> Result<(), String> {
    let win = app
        .get_webview_window("main")
        .ok_or("Main window not found")?;
    // Notify main window frontend to pause canvas animations while minimized
    let _ = win.emit("window-visibility", false);
    win.minimize().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn show_wizard_window(app: AppHandle) -> Result<(), String> {
    let win = app
        .get_webview_window("wizard")
        .ok_or("Wizard window not found")?;
    win.show().map_err(|e| e.to_string())?;
    win.set_focus().map_err(|e| e.to_string())?;
    // Notify wizard frontend to resume canvas animations
    let _ = win.emit("window-visibility", true);
    Ok(())
}

#[tauri::command]
pub fn minimize_wizard(app: AppHandle) -> Result<(), String> {
    let win = app
        .get_webview_window("wizard")
        .ok_or("Wizard window not found")?;
    // Notify wizard frontend to pause canvas animations while minimized
    let _ = win.emit("window-visibility", false);
    win.minimize().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn close_wizard(app: AppHandle) -> Result<(), String> {
    let win = app
        .get_webview_window("wizard")
        .ok_or("Wizard window not found")?;
    // Notify wizard frontend to pause canvas animations before hiding
    let _ = win.emit("window-visibility", false);
    win.hide().map_err(|e| e.to_string())?;
    Ok(())
}
