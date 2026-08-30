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

    // Fixed window size — positioning matches pre-bubble baseline so the
    // bubble is not clipped by the taskbar. Hitbox fix is CSS-only
    // (body pointer-events:none, overlay-root pointer-events:auto) so the
    // transparent 260×146 frame is click-through and does not need HWND resize.
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
pub fn set_overlay_style(app: AppHandle, style: String) -> Result<(), String> {
    let _win = get_overlay_window(&app).ok_or("Overlay window not found")?;
    // BUG-02: keep HWND at fixed 260×146 for correct taskbar clearance and
    // shadow room. Visual style is CSS-only; hitbox is fixed via
    // body{pointer-events:none} + .overlay-root{pointer-events:auto}.
    // Keeping this command is backward-compat for JS that calls it.
    log::debug!(
        "set_overlay_style called: {} — HWND size unchanged (CSS hitbox fix)",
        style
    );
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
pub fn toggle_maximize_main_window(app: AppHandle) -> Result<bool, String> {
    let win = app
        .get_webview_window("main")
        .ok_or("Main window not found")?;
    if win.is_maximized().map_err(|e| e.to_string())? {
        win.unmaximize().map_err(|e| e.to_string())?;
    } else {
        win.maximize().map_err(|e| e.to_string())?;
    }
    // Notify main window frontend to resume canvas animations
    let _ = win.emit("window-visibility", true);
    win.is_maximized().map_err(|e| e.to_string())
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
