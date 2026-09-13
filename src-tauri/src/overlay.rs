// Fluence Windows - Overlay window management
// Controls the floating, always-on-top recording overlay window.

use tauri::{AppHandle, Emitter, Manager, WebviewWindow};

pub fn get_overlay_window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window("overlay")
}

/// Logical HWND size per overlay tier (see src/css/overlay.css geometry).
/// The window shrinks to the tier so the transparent frame cannot swallow
/// clicks around small tiers (Windows has no per-pixel click-through; CSS
/// pointer-events only governs web hit-testing). Each size fits its card +
/// the 46px app-pill reserve + halo wash + bubble badge overhang, with a few
/// px slack for fractional-scale rounding:
/// - full:    320 card + 2x36 halo = 392 -> 400; 46 + 96 + 36 halo = 178 -> 190
/// - compact: 118 card + 2x24 halo = 166 -> 180; 46 + 36 + 24 halo = 106 -> 130
/// - bubble:  44 orb + 16 badge + 2x24 halo = 108 -> 130; 46 + 44 + 16 + 24 = 130 -> 140
fn overlay_window_size(style: &str) -> (f64, f64) {
    match style {
        "compact" => (180.0, 130.0),
        "bubble" => (130.0, 140.0),
        _ => (400.0, 190.0), // full (default)
    }
}

/// Resize + move the overlay window so the tier stays glued to its docked
/// corner. Called while hidden at summon (no flicker) and on style change.
fn place_overlay_window(
    win: &WebviewWindow,
    monitor: &tauri::Monitor,
    position: &str,
    style: &str,
) -> Result<(), String> {
    let (win_width, win_height) = overlay_window_size(style);
    place_overlay_window_sized(win, monitor, position, win_width, win_height)
}

fn place_overlay_window_sized(
    win: &WebviewWindow,
    monitor: &tauri::Monitor,
    position: &str,
    win_width: f64,
    win_height: f64,
) -> Result<(), String> {
    let screen_size = monitor.size();
    let scale = monitor.scale_factor();
    let margin = 20.0;

    let (x, y) = match position {
        "bottom_left" => (
            margin,
            screen_size.height as f64 / scale - win_height - margin - 24.0,
        ),
        "top_left" => (margin, margin),
        "top_center" => (
            screen_size.width as f64 / scale / 2.0 - win_width / 2.0,
            margin,
        ),
        "top_right" => (
            screen_size.width as f64 / scale - win_width - margin,
            margin,
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

    win.set_size(tauri::LogicalSize::new(win_width, win_height))
        .map_err(|e| e.to_string())?;
    win.set_position(tauri::LogicalPosition::new(x, y))
        .map_err(|e| e.to_string())?;
    Ok(())
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

    // Position against the LIVE window size: the JS flow runs set_overlay_style
    // (fresh style) immediately before this command, so the HWND already has
    // the summon's size. Reading the style from the settings file here could
    // lag a just-saved change by the persist debounce and size one summon for
    // the previous tier — the live size cannot be stale. Falls back to the
    // stored style only if the OS size is unreadable.
    let scale = monitor.scale_factor();
    let (win_width, win_height) = win
        .outer_size()
        .map(|s| (s.width as f64 / scale, s.height as f64 / scale))
        .unwrap_or_else(|_| {
            let style = crate::settings::load_settings()
                .map(|s| s.overlay_style)
                .unwrap_or_else(|_| "full".to_string());
            overlay_window_size(&style)
        });
    place_overlay_window_sized(&win, &monitor, &position, win_width, win_height)?;

    win.set_always_on_top(true).map_err(|e| e.to_string())?;
    win.show().map_err(|e| e.to_string())?;
    // Notify overlay frontend to start the waveform animation loop
    let _ = win.emit("window-visibility", true);
    Ok(())
}

#[tauri::command]
pub fn set_overlay_style(app: AppHandle, style: String) -> Result<(), String> {
    let win = get_overlay_window(&app).ok_or("Overlay window not found")?;
    // Shrink/grow the HWND to the tier (plus re-glue to the stored corner)
    // so the transparent frame never extends past the visible card. Called
    // while hidden at summon, so no flicker; keeping this command name is
    // backward-compat for JS that calls it.
    let monitor = win
        .current_monitor()
        .map_err(|e| e.to_string())?
        .or_else(|| win.primary_monitor().ok().flatten())
        .ok_or("No monitor found")?;
    let position = crate::settings::load_settings()
        .map(|s| s.overlay_position)
        .unwrap_or_else(|_| "bottom_right".to_string());
    place_overlay_window(&win, &monitor, &position, &style)?;
    log::debug!("set_overlay_style placed: {} (HWND resized to tier)", style);
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
