// Fluence Windows — Main entry point
// Initializes Tauri v2 app, registers all IPC commands, sets up tray,
// hotkey, SQLite history, and handles first-run wizard detection.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent;
mod audio;
mod autostart;
mod clipboard;
mod credentials;
mod dictionary;
mod history;
mod hotkey;
mod http_client;
mod overlay;
mod settings;
mod tray;
mod transcribe;
mod workflow;
mod offline_downloader;
mod offline_transcribe;

use tauri::Manager;

#[tauri::command]
fn get_app_version(app: tauri::AppHandle) -> String {
    app.package_info().version.to_string()
}

pub fn run() {
    env_logger::init();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            log::info!("Second instance launched. Showing settings window.");
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
                let _ = win.set_focus();
            }
        }))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .setup(|app| {
            // Initialize SQLite history DB
            if let Err(e) = history::init_db() {
                log::error!("Failed to init history DB: {}", e);
            }

            // Set up system tray
            if let Err(e) = tray::setup_tray(app.handle()) {
                log::error!("Failed to set up tray: {}", e);
            }

            // Load settings
            let app_settings = settings::load_settings().unwrap_or_default();

            // Register global hotkeys
            if let Err(e) = hotkey::register_hotkeys(
                app.handle(),
                &app_settings.hotkey,
                &app_settings.recording_mode,
                &app_settings.agent_hotkey,
                &app_settings.agent_recording_mode,
            ) {
                log::warn!("Failed to register hotkeys: {}", e);
            }

            // Determine which window to show on startup
            if app_settings.first_run {
                // Show setup wizard
                if let Some(win) = app.get_webview_window("wizard") {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            } else {
                // App runs in background — main window only shows from tray
                log::info!("Fluence started in background (tray)");
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Settings
            settings::get_settings,
            settings::update_settings,
            // Credentials
            credentials::save_api_key,
            credentials::get_api_key,
            credentials::delete_api_key,
            // Audio
            audio::list_audio_devices,
            audio::start_recording,
            audio::stop_recording,
            audio::is_recording,
            // Transcription
            transcribe::transcribe_audio,
            transcribe::fetch_models,
            transcribe::test_stt_connection,
            // Agent
            agent::execute_agent_command,
            agent::test_llm_connection,
            // Clipboard
            clipboard::inject_text,
            clipboard::execute_keyboard_action,
            clipboard::grab_active_selection,
            // Dictionary
            dictionary::get_dictionary,
            dictionary::add_dictionary_entry,
            dictionary::update_dictionary_entry,
            dictionary::delete_dictionary_entry,
            dictionary::import_dictionary,
            dictionary::export_dictionary,
            // History
            history::get_history,
            history::save_history_entry,
            history::delete_history_entry,
            history::clear_history,
            history::get_history_stats,
            history::get_weekly_activity,
            // Overlay
            overlay::show_overlay,
            overlay::hide_overlay,
            overlay::show_main_window,
            overlay::show_wizard_window,
            overlay::close_wizard,
            overlay::hide_main_window,
            overlay::minimize_main_window,
            overlay::minimize_wizard,
            // Hotkey
            hotkey::update_hotkeys,
            hotkey::get_hotkey_state,
            // End-to-end workflows
            workflow::stop_and_transcribe_recording,
            workflow::finish_transcription_flow,
            // Autostart
            autostart::set_autostart,
            // Offline ASR manager
            offline_downloader::download_offline_model,
            offline_downloader::get_offline_model_status,
            offline_downloader::cancel_offline_download,
            offline_downloader::delete_offline_model,
            // Misc
            get_app_version,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Fluence");

    app.run(|_app, _event| {});
}

fn main() {
    run();
}
