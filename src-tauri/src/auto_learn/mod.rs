// Fluence Windows — Auto-Learn Module
// Contains pipeline-based extraction AND UIA-based post-injection monitoring.
//
// Pipeline extraction (extraction.rs): Compares raw STT output vs final text
// to capture dictionary and AI polish corrections.
//
// UIA monitoring (monitor.rs): After text is pasted into a target app,
// monitors the focused text field for user edits via Windows UI Automation.

pub mod extraction;

#[cfg(target_os = "windows")]
pub mod correction_extractor;
#[cfg(target_os = "windows")]
pub mod learner;
#[cfg(target_os = "windows")]
pub mod monitor;
#[cfg(target_os = "windows")]
pub mod ui_automation;

// Re-export extraction types for use by other modules (workflow.rs, etc.)
pub use extraction::{extract_candidates, Candidate, ExtractionContext, TransformationType};

/// Start the post-injection auto-learn monitor on a dedicated OS thread.
/// Returns immediately. The monitor runs independently for up to 30 seconds.
/// Safe to call from any context; failures are fully isolated and logged.
#[cfg(target_os = "windows")]
pub fn start_post_injection_monitor(injected_text: String) {
    use std::sync::OnceLock;

    /// Channel sender shared across the application.
    /// The receiver lives on the monitor thread.
    static CHANNEL: OnceLock<std::sync::mpsc::Sender<String>> = OnceLock::new();

    let sender = CHANNEL.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<String>();

        // Spawn a dedicated OS thread for UIA monitoring.
        // UI Automation requires STA COM, which is unreliable on thread pools.
        std::thread::Builder::new()
            .name("fluence-auto-learn".to_string())
            .spawn(move || {
                monitor::monitoring_thread(rx);
            })
            .expect("failed to spawn auto-learn monitor thread");

        tx
    });

    if let Err(e) = sender.send(injected_text) {
        log::warn!("[AutoLearn] Failed to send to monitor thread: {}", e);
    }
}

#[cfg(not(target_os = "windows"))]
pub fn start_post_injection_monitor(_injected_text: String) {
    // No-op on non-Windows platforms
}
