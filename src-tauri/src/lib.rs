// Fluence Windows - Library module declarations
#![allow(
    clippy::needless_range_loop,
    clippy::needless_borrow,
    clippy::too_many_arguments
)]

pub mod acl;
pub mod account_scope;
pub mod agent;
pub mod agents;
pub mod app_icon;
pub mod audio;
pub mod auto_learn;
pub mod autostart;
pub mod clipboard;
pub mod credentials;
pub mod dictionary;
pub mod ducking;
pub mod foreground;
pub mod history;
pub mod hotkey;
pub mod http_client;
pub mod installed_apps;
pub mod offline_downloader;
pub mod offline_transcribe;
pub mod overlay;
pub mod prompts;
pub mod settings;
pub mod silence_gate;
pub mod snippets;
pub mod suggestion;
pub mod sync;
pub mod transcribe;
pub mod tray;
pub mod workflow;

#[cfg(test)]
pub mod audit_evidence;
