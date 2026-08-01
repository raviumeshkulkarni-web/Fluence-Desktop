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

#[cfg(target_os = "windows")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(target_os = "windows")]
use std::sync::{mpsc::Sender, OnceLock};

#[cfg(target_os = "windows")]
pub(crate) struct MonitorRequest {
    pub(crate) injected_text: String,
    pub(crate) generation: u64,
}

#[cfg(target_os = "windows")]
static ACTIVE_GENERATION: AtomicU64 = AtomicU64::new(0);

#[cfg(target_os = "windows")]
static CHANNEL: OnceLock<Sender<MonitorRequest>> = OnceLock::new();

/// Start the post-injection auto-learn monitor on a dedicated OS thread.
/// Returns immediately. The monitor runs independently for up to 30 seconds.
/// Safe to call from any context; failures are fully isolated and logged.
#[cfg(target_os = "windows")]
pub fn start_post_injection_monitor(injected_text: String) {
    // A newer injection supersedes any older monitoring session. The worker
    // checks this generation while polling, so rapid hotkey presses cannot
    // leave the latest session waiting behind a 30-second timeout.
    let generation = ACTIVE_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;

    let sender = CHANNEL.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<MonitorRequest>();

        // Spawn a dedicated OS thread for UIA monitoring.
        // UI Automation requires STA COM, which is unreliable on thread pools.
        std::thread::Builder::new()
            .name("fluence-auto-learn".to_string())
            .spawn(move || {
                monitor::monitoring_thread(rx, &ACTIVE_GENERATION);
            })
            .expect("failed to spawn auto-learn monitor thread");

        tx
    });

    if let Err(e) = sender.send(MonitorRequest {
        injected_text,
        generation,
    }) {
        log::warn!("[AutoLearn] Failed to send to monitor thread: {}", e);
    }
}

#[cfg(not(target_os = "windows"))]
pub fn start_post_injection_monitor(_injected_text: String) {
    // No-op on non-Windows platforms
}
