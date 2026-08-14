// Fluence Windows — Settings module
// Manages persistent configuration in a JSON file in the app data directory.

use anyhow::Result;
use dirs::data_local_dir;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(default = "default_preset")]
    pub preset: String, // "groq" | "openai" | "custom" | "mistral"
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_api_key_saved")]
    pub api_key_saved: bool, // whether credential is stored in Credential Manager
}

fn default_preset() -> String {
    "groq".to_string()
}
fn default_base_url() -> String {
    "https://api.groq.com/openai".to_string()
}
fn default_model() -> String {
    "whisper-large-v3".to_string()
}
fn default_api_key_saved() -> bool {
    false
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            preset: default_preset(),
            base_url: default_base_url(),
            model: default_model(),
            api_key_saved: default_api_key_saved(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default = "default_hotkey")]
    pub hotkey: String,
    #[serde(default = "default_recording_mode")]
    pub recording_mode: String, // "push_to_toggle" | "hold_to_record"
    #[serde(default = "default_overlay_position")]
    pub overlay_position: String, // "center" | "bottom_left" | "bottom_right"
    #[serde(default = "default_overlay_style")]
    pub overlay_style: String, // "full" | "compact" | "bubble"
    #[serde(default)]
    pub audio_device_id: Option<String>,
    #[serde(default = "default_stt_provider")]
    pub stt_provider: ProviderConfig,
    #[serde(default = "default_llm_provider")]
    pub llm_provider: ProviderConfig,
    #[serde(default = "default_false")]
    pub auto_start: bool,
    #[serde(default = "default_true")]
    pub sound_on_complete: bool,
    #[serde(default = "default_theme")]
    pub theme: String, // "dark" | "light"
    #[serde(default = "default_agent_mode_threshold_ms")]
    #[allow(dead_code)]
    pub agent_mode_threshold_ms: u64,
    #[serde(default = "default_true")]
    pub first_run: bool,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default = "default_agent_hotkey")]
    pub agent_hotkey: String, // e.g. "Ctrl+Shift+A"
    #[serde(default = "default_agent_recording_mode")]
    pub agent_recording_mode: String, // "push_to_toggle" | "hold_to_record"
    #[serde(default = "default_ai_polish_style")]
    pub ai_polish_style: String,
    #[serde(default = "default_true")]
    pub auto_grab_highlight: bool,
    #[serde(default = "default_true")]
    pub auto_learn_enabled: bool,
    #[serde(default = "default_false")]
    pub duck_enabled: bool,
    #[serde(default = "default_duck_level")]
    pub duck_level: f32,
    #[serde(default = "default_offline_engine")]
    pub offline_engine: String, // "sensevoice" | "moonshine_base"
}

fn default_hotkey() -> String {
    "Ctrl+Shift+Space".to_string()
}
fn default_recording_mode() -> String {
    "push_to_toggle".to_string()
}
fn default_overlay_position() -> String {
    "bottom_right".to_string()
}
fn default_overlay_style() -> String {
    "full".to_string()
}
fn default_false() -> bool {
    false
}
fn default_true() -> bool {
    true
}
fn default_theme() -> String {
    "dark".to_string()
}
fn default_agent_mode_threshold_ms() -> u64 {
    800
}
fn default_language() -> String {
    "en".to_string()
}
fn default_agent_hotkey() -> String {
    "Ctrl+Shift+A".to_string()
}
fn default_agent_recording_mode() -> String {
    "push_to_toggle".to_string()
}
fn default_ai_polish_style() -> String {
    "none".to_string()
}
fn default_duck_level() -> f32 {
    0.0
}
fn default_offline_engine() -> String {
    "sensevoice".to_string()
}

fn default_stt_provider() -> ProviderConfig {
    ProviderConfig {
        preset: "groq".to_string(),
        base_url: "https://api.groq.com/openai".to_string(),
        model: "whisper-large-v3".to_string(),
        api_key_saved: false,
    }
}
fn default_llm_provider() -> ProviderConfig {
    ProviderConfig {
        preset: "groq".to_string(),
        base_url: "https://api.groq.com/openai".to_string(),
        model: "llama-3.3-70b-versatile".to_string(),
        api_key_saved: false,
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            hotkey: default_hotkey(),
            recording_mode: default_recording_mode(),
            overlay_position: default_overlay_position(),
            overlay_style: default_overlay_style(),
            audio_device_id: None,
            stt_provider: default_stt_provider(),
            llm_provider: default_llm_provider(),
            auto_start: default_false(),
            sound_on_complete: default_true(),
            theme: default_theme(),
            agent_mode_threshold_ms: default_agent_mode_threshold_ms(),
            first_run: default_true(),
            language: default_language(),
            agent_hotkey: default_agent_hotkey(),
            agent_recording_mode: default_agent_recording_mode(),
            ai_polish_style: default_ai_polish_style(),
            auto_grab_highlight: default_true(),
            auto_learn_enabled: default_true(),
            duck_enabled: default_false(),
            duck_level: default_duck_level(),
            offline_engine: default_offline_engine(),
        }
    }
}

pub fn settings_path() -> PathBuf {
    let mut path = data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("Fluence");
    path.push("settings.json");
    path
}

pub fn load_settings() -> Result<AppSettings> {
    let path = settings_path();
    log::debug!("Loading settings from path: {:?}", path);
    if !path.exists() {
        log::info!("Settings file does not exist, creating default settings");
        let settings = AppSettings::default();
        save_settings(&settings)?;
        return Ok(settings);
    }
    let data = fs::read_to_string(&path)?;
    match serde_json::from_str::<AppSettings>(&data) {
        Ok(settings) => {
            log::debug!("Successfully loaded settings");
            Ok(settings)
        }
        Err(e) => {
            log::error!("Failed to deserialize settings JSON: {:?}", e);
            Err(e.into())
        }
    }
}

pub fn save_settings(settings: &AppSettings) -> Result<()> {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(settings)?;
    fs::write(&path, data)?;
    Ok(())
}

// Tauri commands
#[tauri::command]
pub fn get_settings() -> Result<AppSettings, String> {
    load_settings().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn update_settings(settings: AppSettings) -> Result<(), String> {
    save_settings(&settings).map_err(|e| e.to_string())
}
