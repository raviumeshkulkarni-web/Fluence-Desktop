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

/// Top-left of a window of `win_width` x `win_height` for a dock, in logical
/// pixels. Pure so the geometry both placements depend on is testable.
///
/// Top docks return a `y` that is CONSTANT with respect to height (the window
/// grows downward, away from the docked edge). Bottom docks return a `y`
/// DERIVED from height (`screen_height - win_height - …`, the window grows
/// upward, away from the docked edge) — which is why their geometry must be
/// committed together with the size, see `commit_overlay_geometry`.
fn overlay_window_origin(
    screen_width: f64,
    screen_height: f64,
    position: &str,
    win_width: f64,
    win_height: f64,
) -> (f64, f64) {
    let margin = 20.0;
    let bottom_reserve = 24.0;
    match position {
        "bottom_left" => (margin, screen_height - win_height - margin - bottom_reserve),
        "top_left" => (margin, margin),
        "top_center" => (screen_width / 2.0 - win_width / 2.0, margin),
        "top_right" => (screen_width - win_width - margin, margin),
        "center" => (
            screen_width / 2.0 - win_width / 2.0,
            screen_height - win_height - margin - bottom_reserve,
        ),
        _ => {
            // bottom_right (default)
            (
                screen_width - win_width - margin,
                screen_height - win_height - margin - bottom_reserve,
            )
        }
    }
}

/// Commit the overlay's new position AND size in ONE native operation.
///
/// `set_size()` then `set_position()` are two separate dispatches to the event
/// loop, and a resize is applied with `SWP_NOMOVE` (the current top-left is
/// pinned and the bottom edge extends). For bottom docks, whose `y` is derived
/// from the requested height, the window therefore sits at the OLD `y` with the
/// NEW height until the second dispatch is processed — a transient geometry the
/// compositor can present, and which every bottom-anchored element (island,
/// pill, strip, card) rides. Top docks are immune: their `y` is a constant, so
/// the second dispatch is a no-op and the intermediate state equals the final
/// one.
///
/// A single `SetWindowPos` commits position and size together, so that
/// intermediate geometry never exists. Final coordinates, z-order and
/// activation behaviour are identical to the previous two-call sequence.
#[cfg(windows)]
fn commit_overlay_geometry(
    win: &WebviewWindow,
    x: f64,
    y: f64,
    win_width: f64,
    win_height: f64,
    scale: f64,
) -> Result<(), String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER};

    // Tauri re-exports an HWND from its own `windows` crate version, which is
    // not the one this crate depends on. Both are `pub struct HWND(pub *mut
    // c_void)` over the same OS handle, and HWND carries no ownership or Drop
    // semantics, so re-wrapping the raw handle is sound.
    let raw = win.hwnd().map_err(|e| e.to_string())?.0;
    let hwnd = HWND(raw);
    if hwnd.is_invalid() {
        return Err("Overlay window handle is invalid".to_string());
    }

    // SetWindowPos takes physical device pixels. `scale` is the same monitor
    // scale the logical origin above was derived from, so the conversion is
    // self-consistent with the placement math.
    let to_px = |v: f64| -> i32 { (v * scale).round() as i32 };
    let w_px = (win_width * scale).round().max(1.0) as i32;
    let h_px = (win_height * scale).round().max(1.0) as i32;

    // SWP_NOZORDER | SWP_NOACTIVATE matches the flags tao itself uses when it
    // applies a combined position+size request.
    unsafe {
        SetWindowPos(
            hwnd,
            HWND::default(),
            to_px(x),
            to_px(y),
            w_px,
            h_px,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
        .map_err(|e| e.to_string())
    }
}

/// Non-Windows fallback: the platform has no single-call equivalent exposed
/// here, so keep the original two-call sequence. Windows uses the atomic path
/// above.
#[cfg(not(windows))]
fn commit_overlay_geometry(
    win: &WebviewWindow,
    x: f64,
    y: f64,
    win_width: f64,
    win_height: f64,
    _scale: f64,
) -> Result<(), String> {
    win.set_size(tauri::LogicalSize::new(win_width, win_height))
        .map_err(|e| e.to_string())?;
    win.set_position(tauri::LogicalPosition::new(x, y))
        .map_err(|e| e.to_string())
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
    let (x, y) = overlay_window_origin(
        screen_size.width as f64 / scale,
        screen_size.height as f64 / scale,
        position,
        win_width,
        win_height,
    );
    commit_overlay_geometry(win, x, y, win_width, win_height, scale)
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
    let monitor = win
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| win.primary_monitor().ok().flatten());
    if let Some(monitor) = monitor {
        if let Ok(settings) = crate::settings::load_settings() {
            let _ = place_overlay_window(
                &win,
                &monitor,
                &settings.overlay_position,
                &settings.overlay_style,
            );
        }
    }
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
    // Notify wizard frontend to stop the waveform animation loop
    let _ = win.emit("window-visibility", false);
    win.hide().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::overlay_window_origin;

    const SCREEN_W: f64 = 2560.0;
    const SCREEN_H: f64 = 1440.0;
    const W: f64 = 400.0;
    const COLLAPSED_H: f64 = 228.0;
    const EXPANDED_H: f64 = 408.0;

    fn top_docks() -> [&'static str; 3] {
        ["top_left", "top_center", "top_right"]
    }

    fn bottom_docks() -> [&'static str; 3] {
        ["bottom_left", "center", "bottom_right"]
    }

    /// TOP docks grow downward, so `y` must not depend on height. This is what
    /// made them immune to the old two-call resize.
    #[test]
    fn top_dock_origin_y_is_independent_of_window_height() {
        for dock in top_docks() {
            let (_, y_collapsed) = overlay_window_origin(SCREEN_W, SCREEN_H, dock, W, COLLAPSED_H);
            let (_, y_expanded) = overlay_window_origin(SCREEN_W, SCREEN_H, dock, W, EXPANDED_H);
            assert_eq!(y_collapsed, y_expanded, "{dock} y moved with height");
            assert_eq!(y_collapsed, 20.0, "{dock} y must stay at the top margin");
        }
    }

    /// BOTTOM docks grow upward, so `y` must move up by exactly the height
    /// delta — the window's bottom edge stays glued to the screen. This is why
    /// position and size have to be committed together.
    #[test]
    fn bottom_dock_grows_upward_keeping_the_bottom_edge_fixed() {
        for dock in bottom_docks() {
            let (_, y_collapsed) = overlay_window_origin(SCREEN_W, SCREEN_H, dock, W, COLLAPSED_H);
            let (_, y_expanded) = overlay_window_origin(SCREEN_W, SCREEN_H, dock, W, EXPANDED_H);
            let delta = EXPANDED_H - COLLAPSED_H;
            assert_eq!(
                y_collapsed - y_expanded,
                delta,
                "{dock} must move up by the full height delta"
            );
            let bottom_collapsed = y_collapsed + COLLAPSED_H;
            let bottom_expanded = y_expanded + EXPANDED_H;
            assert_eq!(
                bottom_collapsed, bottom_expanded,
                "{dock} bottom edge must be invariant"
            );
            assert_eq!(bottom_collapsed, SCREEN_H - 44.0, "{dock} bottom reserve");
        }
    }

    /// Horizontal placement is unchanged by the atomic-commit refactor: left
    /// docks hug the left margin, right docks hug the right margin, and centre
    /// docks stay centred.
    #[test]
    fn horizontal_origins_match_each_dock() {
        let margin = 20.0;
        assert_eq!(
            overlay_window_origin(SCREEN_W, SCREEN_H, "top_left", W, COLLAPSED_H).0,
            margin
        );
        assert_eq!(
            overlay_window_origin(SCREEN_W, SCREEN_H, "bottom_left", W, COLLAPSED_H).0,
            margin
        );
        assert_eq!(
            overlay_window_origin(SCREEN_W, SCREEN_H, "top_right", W, COLLAPSED_H).0,
            SCREEN_W - W - margin
        );
        assert_eq!(
            overlay_window_origin(SCREEN_W, SCREEN_H, "bottom_right", W, COLLAPSED_H).0,
            SCREEN_W - W - margin
        );
        for dock in ["top_center", "center"] {
            assert_eq!(
                overlay_window_origin(SCREEN_W, SCREEN_H, dock, W, COLLAPSED_H).0,
                SCREEN_W / 2.0 - W / 2.0,
                "{dock} must stay horizontally centred"
            );
        }
    }

    /// An unknown dock falls back to bottom_right, preserving the original
    /// catch-all arm.
    #[test]
    fn unknown_position_falls_back_to_bottom_right() {
        assert_eq!(
            overlay_window_origin(SCREEN_W, SCREEN_H, "something_new", W, COLLAPSED_H),
            overlay_window_origin(SCREEN_W, SCREEN_H, "bottom_right", W, COLLAPSED_H)
        );
    }
}
