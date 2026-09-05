// Fluence Windows — Offline Transcriber Module
// Manages sherpa-onnx sidecar lifecycle and WebSocket communication.

use anyhow::{anyhow, Result};
use futures_util::{SinkExt, StreamExt};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfflineEngine {
    SenseVoice,
    #[serde(rename = "moonshine_v2_small")]
    MoonshineV2Small,
    #[serde(rename = "moonshine_v2_medium")]
    MoonshineV2Medium,
}

impl OfflineEngine {
    pub fn display_name(&self) -> &str {
        match self {
            Self::SenseVoice => "SenseVoice",
            Self::MoonshineV2Small => "Moonshine v2 Small (English)",
            Self::MoonshineV2Medium => "Moonshine v2 Medium (English)",
        }
    }

    pub fn dir_name(&self) -> &str {
        match self {
            Self::SenseVoice => "sensevoice_v2",
            Self::MoonshineV2Small => "moonshine_v2_small",
            Self::MoonshineV2Medium => "moonshine_v2_medium",
        }
    }

    /// Streaming-architecture id for the v2 sidecar ("small" | "medium"),
    /// matching the official `MOONSHINE_MODEL_ARCH_*_STREAMING` ids and the
    /// v2 manifests. `None` for the sherpa-onnx engines.
    pub fn v2_arch(&self) -> Option<&'static str> {
        match self {
            Self::MoonshineV2Small => Some("small"),
            Self::MoonshineV2Medium => Some("medium"),
            _ => None,
        }
    }
}

impl std::fmt::Display for OfflineEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SenseVoice => write!(f, "sensevoice"),
            Self::MoonshineV2Small => write!(f, "moonshine_v2_small"),
            Self::MoonshineV2Medium => write!(f, "moonshine_v2_medium"),
        }
    }
}

impl std::str::FromStr for OfflineEngine {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "sensevoice" => Ok(Self::SenseVoice),
            "moonshine_v2_small" => Ok(Self::MoonshineV2Small),
            "moonshine_v2_medium" => Ok(Self::MoonshineV2Medium),
            _ => Err(anyhow!("Unknown offline engine: {}", s)),
        }
    }
}

// Global process and port handles
struct ServerInstance {
    child: Child,
    port: u16,
    engine: OfflineEngine,
}
static SERVER_INSTANCE: Lazy<Mutex<Option<ServerInstance>>> = Lazy::new(|| Mutex::new(None));
static LAST_USED: Lazy<Mutex<Instant>> = Lazy::new(|| Mutex::new(Instant::now()));
static IDLE_MONITOR_RUNNING: Lazy<AtomicBool> = Lazy::new(|| AtomicBool::new(false));
static SERVER_START_LOCK: Lazy<tokio::sync::Mutex<()>> = Lazy::new(|| tokio::sync::Mutex::new(()));

fn is_port_available(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

fn start_idle_monitor() {
    if IDLE_MONITOR_RUNNING.swap(true, Ordering::SeqCst) {
        return; // Already running
    }

    tokio::spawn(async {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;

            let last_used = {
                let lock = LAST_USED.lock().unwrap();
                *lock
            };

            if last_used.elapsed() > std::time::Duration::from_secs(180) {
                // 3 minutes
                log::info!("Idle timeout reached. Shutting down sherpa-onnx server.");
                let mut lock = SERVER_INSTANCE.lock().unwrap();
                if let Some(mut instance) = lock.take() {
                    let _ = instance.child.kill();
                }
                IDLE_MONITOR_RUNNING.store(false, Ordering::SeqCst);
                break;
            }
        }
    });
}

/// Target triple suffix used for the Tauri externalBin staging name
/// (`binaries/moonshine-v2-server-<triple>.exe`). The app ships Windows
/// x86_64 only; anything else resolves to a sentinel that matches nothing.
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
const V2_SIDECAR_TRIPLE: &str = "x86_64-pc-windows-msvc";
#[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
const V2_SIDECAR_TRIPLE: &str = "unknown-target";

/// Ordered candidate locations for the bundled Moonshine v2 sidecar exe,
/// given the app-exe directory, the crate (src-tauri) directory, and the
/// cargo target directory. Pure function so the ordering is unit-testable.
///
/// Candidate order:
///
/// 1. Beside the running app exe — production NSIS layout, where Tauri
///    externalBin files land next to the installed application binary.
/// 2. Workspace cargo output (release, then debug) — local dev builds.
/// 3. Tauri binaries/ staging dir with the target-triple suffix — the exact
///    file externalBin consumes at packaging time.
fn v2_sidecar_candidates(
    exe_dir: Option<&std::path::Path>,
    manifest_dir: &std::path::Path,
    target_dir: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    let mut out = Vec::with_capacity(4);
    if let Some(dir) = exe_dir {
        out.push(dir.join(crate::offline_downloader::MOONSHINE_V2_SERVER_EXE));
    }
    for profile in ["release", "debug"] {
        out.push(
            target_dir
                .join(profile)
                .join(crate::offline_downloader::MOONSHINE_V2_SERVER_EXE),
        );
    }
    out.push(manifest_dir.join(format!(
        "binaries/moonshine-v2-server-{V2_SIDECAR_TRIPLE}.exe"
    )));
    out
}

/// Resolves the shipped Moonshine v2 sidecar exe plus its sibling
/// onnxruntime.dll (Windows implicit DLL search starts at the loading
/// executable's own directory, so the dll must sit beside the exe —
/// resource-bundled in prod, build-staged in dev).
fn resolve_v2_sidecar() -> Result<std::path::PathBuf> {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    let target_dir = manifest_dir.join("target");
    let candidates = v2_sidecar_candidates(exe_dir.as_deref(), &manifest_dir, &target_dir);
    for exe in &candidates {
        if exe.is_file() {
            let dll = exe
                .parent()
                .map(|d| d.join(crate::offline_downloader::MOONSHINE_V2_ORT_DLL));
            match dll {
                Some(d) if d.is_file() => return Ok(exe.clone()),
                _ => {
                    return Err(anyhow!(
                        "Moonshine v2 sidecar found at {} but its onnxruntime.dll sibling is missing; reinstall the app or rebuild the sidecar.",
                        exe.display()
                    ))
                }
            }
        }
    }
    Err(anyhow!(
        "Moonshine v2 sidecar not found. Searched: {}. The runtime ships with the app installer; on dev builds run `cargo build -p moonshine-v2-server` first.",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

pub async fn ensure_server_running(engine: OfflineEngine) -> Result<u16> {
    let _start_lock = SERVER_START_LOCK.lock().await;
    // Check if an existing server is running with the same engine
    {
        let mut lock = SERVER_INSTANCE.lock().unwrap();
        if let Some(ref mut instance) = *lock {
            if instance.engine == engine {
                match instance.child.try_wait() {
                    Ok(None) => {
                        // Still running with same engine, update last used timestamp
                        *LAST_USED.lock().unwrap() = Instant::now();
                        return Ok(instance.port);
                    }
                    _ => {
                        // Process exited
                        *lock = None;
                    }
                }
            } else {
                // Different engine requested — kill the current server
                log::info!(
                    "Switching engine from {} to {}, killing current server",
                    instance.engine.display_name(),
                    engine.display_name()
                );
                let _ = instance.child.kill();
                *lock = None;
            }
        }
    }
    // Brief pause to let port release (lock must be dropped before await)
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let offline_dir = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("Fluence")
        .join("bin")
        .join(engine.dir_name());

    let exe_name = match engine.v2_arch() {
        Some(_) => crate::offline_downloader::MOONSHINE_V2_SERVER_EXE,
        None => "sherpa-onnx-offline-websocket-server.exe",
    };

    // Moonshine v2 ships in the installer (Tauri externalBin) or a dev
    // build tree — never in the model dir (see resolve_v2_sidecar). The
    // sherpa runtime is still downloaded into its model dir by precedent.
    let (exe_path, runtime_dir) = match engine.v2_arch() {
        Some(_) => {
            let resolved = resolve_v2_sidecar()?;
            let dir = resolved
                .parent()
                .ok_or_else(|| anyhow!("Resolved sidecar path has no parent directory"))?
                .to_path_buf();
            (resolved, dir)
        }
        None => (offline_dir.join(exe_name), offline_dir.clone()),
    };

    if !exe_path.exists() || !runtime_dir.join("onnxruntime.dll").exists() {
        return Err(anyhow!(
            "Offline transcription engine is not installed. Please download it in Settings."
        ));
    }

    // Verify binary integrity before execution. Fail closed: with no pin
    // available anywhere, refuse to execute native code rather than
    // warn-and-continue. Pin precedence: compile-time hash of the staged
    // artifact (baked by build.rs, covers every dev/CI build) over the
    // manifest pin.
    match engine.v2_arch() {
        Some(arch) => {
            let manifest = crate::offline_downloader::moonshine_v2_manifest_for_arch(arch)?;
            let expected = option_env!("MOONSHINE_V2_SERVER_SHA256")
                .map(str::to_string)
                .or(manifest.server_exe_sha256.clone())
                .ok_or_else(|| anyhow!(
                    "Moonshine v2 sidecar has no pinned hash (rebuild the sidecar so build.rs can pin it, or reinstall the app)."
                ))?;
            crate::offline_downloader::verify_sha256(&exe_path, &expected).map_err(|e| {
                anyhow!(
                    "Binary integrity check failed: {}. Please re-download the model in Settings.",
                    e
                )
            })?;
        }
        None => {
            let exe_hash = crate::offline_downloader::manifest_binary_hash(
                "sherpa-onnx-offline-websocket-server.exe",
            )?;
            crate::offline_downloader::verify_sha256(&exe_path, exe_hash).map_err(|e| {
                anyhow!(
                    "Binary integrity check failed: {}. Please re-download the model in Settings.",
                    e
                )
            })?;
        }
    }

    // Verify model files exist for the requested engine
    match engine {
        OfflineEngine::SenseVoice => {
            let model_path = offline_dir.join("model.int8.onnx");
            let tokens_path = offline_dir.join("tokens.txt");
            if !model_path.exists() || !tokens_path.exists() {
                return Err(anyhow!(
                    "SenseVoice model files are not installed. Please download them in Settings."
                ));
            }
        }
        engine @ (OfflineEngine::MoonshineV2Small | OfflineEngine::MoonshineV2Medium) => {
            let arch = engine.v2_arch().unwrap_or("small");
            let manifest = crate::offline_downloader::moonshine_v2_manifest_for_arch(arch)?;
            for file in &manifest.files {
                let p = offline_dir.join(&file.name);
                if !p.exists() {
                    return Err(anyhow!(
                        "Moonshine v2 model file '{}' is missing. Please download the model in Settings.",
                        file.name
                    ));
                }
            }
        }
    }

    // Find free port
    let mut port = 6006;
    for p in 6006..=6029 {
        if is_port_available(p) {
            port = p;
            break;
        }
    }
    let server_label = match engine.v2_arch() {
        Some(_) => "moonshine-v2",
        None => "sherpa-onnx",
    };
    log::info!(
        "Starting {} websocket server (engine: {}) on port {}",
        server_label,
        engine.display_name(),
        port
    );

    let port_arg = format!("--port={}", port);
    let threads_arg = "--num-threads=3".to_string();

    let mut cmd = Command::new(&exe_path);
    cmd.current_dir(&offline_dir);
    cmd.creation_flags(CREATE_NO_WINDOW);

    match engine {
        OfflineEngine::SenseVoice => {
            let model_path = offline_dir.join("model.int8.onnx");
            let tokens_path = offline_dir.join("tokens.txt");
            let tokens_arg = format!("--tokens={}", tokens_path.display().to_string());
            let model_arg = format!("--sense-voice-model={}", model_path.display().to_string());
            cmd.args([&tokens_arg, &model_arg, &port_arg, &threads_arg]);
        }
        OfflineEngine::MoonshineV2Small | OfflineEngine::MoonshineV2Medium => {
            // Served by our own sidecar: same WS protocol as the sherpa
            // server, model dir + streaming arch as CLI args. The Moonshine
            // core manages its own threading, so no --num-threads is passed.
            let arch = engine.v2_arch().unwrap_or("small");
            let model_dir_arg = format!("--model-dir={}", offline_dir.display());
            let arch_arg = format!("--arch={arch}");
            cmd.args([&model_dir_arg, &arch_arg, &port_arg]);
        }
    }

    {
        let mut lock = SERVER_INSTANCE.lock().unwrap();
        let child = cmd
            .spawn()
            .map_err(|e| anyhow!("Failed to spawn ASR server: {}", e))?;
        *lock = Some(ServerInstance {
            child,
            port,
            engine,
        });
        *LAST_USED.lock().unwrap() = Instant::now();
    }

    start_idle_monitor();

    // Perform health checks
    let mut ready = false;
    for _ in 0..30 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            ready = true;
            break;
        }
    }

    if !ready {
        let mut lock = SERVER_INSTANCE.lock().unwrap();
        if let Some(mut instance) = lock.take() {
            let _ = instance.child.kill();
        }
        return Err(anyhow!(
            "{} server failed to start on port {}",
            server_label,
            port
        ));
    }

    log::info!(
        "{} server is ready on port {} (engine: {})",
        server_label,
        port,
        engine.display_name()
    );
    Ok(port)
}

pub async fn transcribe_samples(samples: &[f32], engine: OfflineEngine) -> Result<String> {
    if samples.len() > crate::transcribe::MAX_OFFLINE_SAMPLES {
        return Err(anyhow!(
            "Recording too long ({} samples, {:.1} minutes). Maximum is 10 minutes for offline mode.",
            samples.len(),
            samples.len() as f64 / 16_000.0 / 60.0
        ));
    }
    let port = ensure_server_running(engine).await?;
    *LAST_USED.lock().unwrap() = Instant::now();

    let url = format!("ws://127.0.0.1:{}", port);
    let (mut ws_stream, _) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async(&url),
    )
    .await
    .map_err(|_| anyhow!("Timed out connecting to local ASR server"))?
    .map_err(|e| anyhow!("Failed to connect to local ASR server: {}", e))?;

    // Build the custom sherpa-onnx binary payload
    // Format: [int32LE sample_rate][int32LE num_audio_bytes][float32 samples...]
    let sample_rate = 16000u32;
    let byte_len = (samples.len() * 4) as u32;
    let mut payload = Vec::with_capacity(8 + samples.len() * 4);
    payload.extend_from_slice(&sample_rate.to_le_bytes());
    payload.extend_from_slice(&byte_len.to_le_bytes());
    for &sample in samples {
        payload.extend_from_slice(&sample.to_le_bytes());
    }

    // Send audio payload
    tokio::time::timeout(
        Duration::from_secs(10),
        ws_stream.send(tokio_tungstenite::tungstenite::Message::Binary(payload)),
    )
    .await
    .map_err(|_| anyhow!("Timed out sending audio to local ASR server"))?
    .map_err(|e| anyhow!("Failed to send audio to ASR server: {}", e))?;

    // Per sherpa-onnx protocol: server decodes after receiving all bytes,
    // sends text result, THEN client sends "Done" to close.
    // Do NOT send "Done" before receiving the result — it closes the connection.

    let receive_result = tokio::time::timeout(Duration::from_secs(120), async {
        let mut result_text = String::new();
        while let Some(msg) = ws_stream.next().await {
            let msg = msg.map_err(|e| anyhow!("Error receiving from ASR server: {}", e))?;
            match msg {
                tokio_tungstenite::tungstenite::Message::Text(text) => {
                    result_text = text;
                    break;
                }
                tokio_tungstenite::tungstenite::Message::Close(_) => {
                    break;
                }
                _ => {}
            }
        }
        Ok::<String, anyhow::Error>(result_text)
    })
    .await;

    let result_text = match receive_result {
        Ok(result) => result?,
        Err(_) => {
            let _ = ws_stream.close(None).await;
            return Err(anyhow!("Timed out waiting for local ASR server response"));
        }
    };

    // Now send "Done" to cleanly close the connection
    let _ = ws_stream
        .send(tokio_tungstenite::tungstenite::Message::Text(
            "Done".to_string(),
        ))
        .await;
    let _ = ws_stream.close(None).await;

    #[derive(serde::Deserialize)]
    struct AsrResponse {
        text: Option<String>,
    }

    if result_text.is_empty() {
        return Err(anyhow!("Empty response from ASR server"));
    }

    // If it's valid JSON, extract text. Otherwise, use raw string.
    let response: AsrResponse =
        serde_json::from_str(&result_text).unwrap_or_else(|_| AsrResponse {
            text: Some(result_text.trim().to_string()),
        });

    let text = response.text.unwrap_or_default().trim().to_string();
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn engine_display_parse_round_trip() {
        for engine in [
            OfflineEngine::SenseVoice,
            OfflineEngine::MoonshineV2Small,
            OfflineEngine::MoonshineV2Medium,
        ] {
            let s = engine.to_string();
            assert_eq!(OfflineEngine::from_str(&s).unwrap(), engine);
        }
    }

    #[test]
    fn engine_parse_rejects_unknown() {
        assert!(OfflineEngine::from_str("moonshine_v2_large").is_err());
        assert!(OfflineEngine::from_str("").is_err());
        // Retired v1 batch id: legacy settings files are migrated to
        // moonshine_v2_small at load (see migrate_retired_offline_engine
        // in settings.rs), so the engine itself must no longer accept it.
        assert!(OfflineEngine::from_str("moonshine_base").is_err());
    }

    #[test]
    fn v2_arch_mapping_matches_manifests_and_official_ids() {
        assert_eq!(OfflineEngine::MoonshineV2Small.v2_arch(), Some("small"));
        assert_eq!(OfflineEngine::MoonshineV2Medium.v2_arch(), Some("medium"));
        assert_eq!(OfflineEngine::SenseVoice.v2_arch(), None);
    }

    #[test]
    fn v2_engine_dirs_do_not_collide_with_existing_engines() {
        let dirs = [
            OfflineEngine::SenseVoice.dir_name(),
            OfflineEngine::MoonshineV2Small.dir_name(),
            OfflineEngine::MoonshineV2Medium.dir_name(),
        ];
        let mut unique = dirs.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), dirs.len());
        assert_eq!(
            OfflineEngine::MoonshineV2Small.dir_name(),
            "moonshine_v2_small"
        );
        assert_eq!(
            OfflineEngine::MoonshineV2Medium.dir_name(),
            "moonshine_v2_medium"
        );
    }

    #[test]
    fn v2_sidecar_candidates_prefer_installed_bundle_over_dev_trees() {
        use std::path::PathBuf;
        let exe_dir = PathBuf::from("C:/install");
        let manifest_dir = PathBuf::from("D:/repo/src-tauri");
        let target_dir = PathBuf::from("D:/repo/src-tauri/target");
        let got = v2_sidecar_candidates(Some(exe_dir.as_path()), &manifest_dir, &target_dir);
        let names: Vec<String> = got
            .iter()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(
            names,
            vec![
                "C:/install/moonshine-v2-server.exe".to_string(),
                "D:/repo/src-tauri/target/release/moonshine-v2-server.exe".to_string(),
                "D:/repo/src-tauri/target/debug/moonshine-v2-server.exe".to_string(),
                format!("D:/repo/src-tauri/binaries/moonshine-v2-server-{V2_SIDECAR_TRIPLE}.exe"),
            ]
        );
    }

    #[test]
    fn v2_sidecar_candidates_survive_missing_exe_dir() {
        use std::path::PathBuf;
        let manifest_dir = PathBuf::from("D:/repo/src-tauri");
        let target_dir = PathBuf::from("D:/repo/src-tauri/target");
        let got = v2_sidecar_candidates(None, &manifest_dir, &target_dir);
        // No app-exe dir (e.g. test harness): dev candidates still listed.
        assert_eq!(got.len(), 3);
    }
}
