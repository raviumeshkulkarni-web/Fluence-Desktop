// Fluence Windows - Offline Transcriber Module
// Manages sherpa-onnx sidecar lifecycle and WebSocket communication.

use anyhow::{anyhow, Result};
use futures_util::{SinkExt, StreamExt};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::io::Write;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Hide the sidecar console window on Windows. No equivalent needed on
/// Linux (no console window is created for spawned processes).
#[cfg(target_os = "windows")]
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
    // Loopback shared secret handed to our moonshine sidecar at spawn.
    // None for the sherpa sidecar (which predates the token protocol).
    auth_token: Option<String>,
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
                    forget_sidecar_pid();
                }
                IDLE_MONITOR_RUNNING.store(false, Ordering::SeqCst);
                break;
            }
        }
    });
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
const V2_SIDECAR_TRIPLE: &str = "x86_64-pc-windows-msvc";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const V2_SIDECAR_TRIPLE: &str = "x86_64-unknown-linux-gnu";
#[cfg(not(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
)))]
const V2_SIDECAR_TRIPLE: &str = "unknown-target";

#[cfg(target_os = "windows")]
const V2_SIDECAR_BIN_SUFFIX: &str = ".exe";
#[cfg(not(target_os = "windows"))]
const V2_SIDECAR_BIN_SUFFIX: &str = "";

/// Ordered candidate locations for the bundled Moonshine v2 sidecar exe,
/// given the app-exe directory, the crate (src-tauri) directory, and the
/// cargo target directory. Pure function so the ordering is unit-testable.
///
/// Candidate order:
///
/// 1. Beside the running app exe - production NSIS layout, where Tauri
///    externalBin files land next to the installed application binary.
/// 2. Workspace cargo output (release, then debug) - local dev builds.
/// 3. Tauri binaries/ staging dir with the target-triple suffix - the exact
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
        "binaries/moonshine-v2-server-{V2_SIDECAR_TRIPLE}{V2_SIDECAR_BIN_SUFFIX}"
    )));
    out
}

/// Ordered DLL lookup locations for a given sidecar exe path (pure function
/// so the ordering is unit-testable).
///
/// 1. Beside the exe — dev trees (`target/release|debug`), self-contained
///    model dirs, manual installs.
/// 2. `<exe_dir>/binaries/` — production NSIS layout. Tauri installs the
///    `binaries/onnxruntime.dll` resource preserving its declared relative
///    path, while `externalBin` lands flat next to the app exe, so a stock
///    install always splits the pair across these two spots.
fn v2_sidecar_dll_candidates(exe_path: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::with_capacity(2);
    if let Some(dir) = exe_path.parent() {
        out.push(dir.join(crate::offline_downloader::MOONSHINE_V2_ORT_DLL));
        out.push(
            dir.join("binaries")
                .join(crate::offline_downloader::MOONSHINE_V2_ORT_DLL),
        );
    }
    out
}

/// Resolves the shipped Moonshine v2 sidecar exe plus its sibling
/// onnxruntime.dll (Windows implicit DLL search starts at the loading
/// executable's own directory, so the dll must sit beside the exe -
/// resource-bundled in prod, build-staged in dev). The production NSIS
/// layout splits the pair: `externalBin` lands flat next to the app exe
/// while the `binaries/onnxruntime.dll` resource keeps its declared
/// relative path under `<install>/binaries/`. Both spots are accepted
/// (see `v2_sidecar_dll_candidates`); an exe without a usable DLL never
/// shadows a healthy candidate later in the list.
fn resolve_v2_sidecar() -> Result<std::path::PathBuf> {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    let target_dir = manifest_dir.join("target");
    let candidates = v2_sidecar_candidates(exe_dir.as_deref(), &manifest_dir, &target_dir);
    // Exe copies without a usable DLL must not shadow healthy ones (e.g. a
    // stale production copy), so a missing DLL continues the scan instead
    // of returning early. The final error still names every partial hit.
    let mut dll_less: Vec<String> = Vec::new();
    for exe in &candidates {
        if !exe.is_file() {
            continue;
        }
        let dll_found = v2_sidecar_dll_candidates(exe).iter().any(|d| d.is_file());
        if !dll_found {
            dll_less.push(format!(
                "{} (no {} beside it or in its binaries/ subdir)",
                exe.display(),
                crate::offline_downloader::MOONSHINE_V2_ORT_DLL
            ));
            continue;
        }
        return Ok(exe.clone());
    }
    if !dll_less.is_empty() {
        return Err(anyhow!(
            "Moonshine v2 sidecar found but its {} sibling is missing: {}. Reinstall the app or rebuild the sidecar.",
            crate::offline_downloader::MOONSHINE_V2_ORT_DLL,
            dll_less.join("; ")
        ));
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

/// Fresh loopback secret shared with the moonshine sidecar at spawn.
/// CONTRACT: exactly 32 lowercase hex characters — the sidecar requires
/// this length (moonshine-v2-server::TOKEN_LEN, pinned by its wire test);
/// a change here must land together with the sidecar half.
fn generate_spawn_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum ProbeOutcome {
    /// The spawned sidecar answered the authenticated handshake.
    Ready,
    /// Nothing is listening yet - normal while the model loads.
    NotYet,
    /// Something answered the TCP/WebSocket connect but never produced the
    /// named reply. A stale sidecar build (predating the token protocol)
    /// behaves exactly like this, and retrying cannot help: it will never
    /// answer. Fail fast so the user gets a real error instead of hanging.
    Incompatible,
}

/// Authenticated readiness probe for the moonshine sidecar. A bare TCP
/// connect does NOT prove that the spawned sidecar owns the port — any
/// local process could hold it first and pass the health check while the
/// real sidecar dies on its own bind. This completes the WebSocket
/// handshake and sends `probe:<token>`; only the sidecar, which knows the
/// fresh spawn secret, can answer with the named reply.
/// Note: the Sherpa sidecar keeps the bare-TCP readiness contract
/// (documented residual risk until it receives a compatible fix).
async fn probe_moonshine_ready(port: u16, token: &str) -> ProbeOutcome {
    let url = format!("ws://127.0.0.1:{port}");
    let (mut ws, _) = match tokio::time::timeout(
        Duration::from_secs(5),
        tokio_tungstenite::connect_async(&url),
    )
    .await
    {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            // Almost always "nothing is listening yet": the sidecar reads its
            // stdin token and loads the model BEFORE it binds the port.
            log::debug!("readiness probe {port}: connect failed: {e}");
            return ProbeOutcome::NotYet;
        }
        Err(_) => {
            log::debug!("readiness probe {port}: connect timed out after 5s");
            return ProbeOutcome::NotYet;
        }
    };
    let probe = tokio_tungstenite::tungstenite::Message::Text(format!("probe:{token}"));
    if tokio::time::timeout(Duration::from_secs(2), ws.send(probe))
        .await
        .is_err()
    {
        log::warn!("readiness probe {port}: sending probe timed out");
        let _ = ws.close(None).await;
        return ProbeOutcome::Incompatible;
    }
    let reply = tokio::time::timeout(Duration::from_secs(2), async {
        let mut text: Option<String> = None;
        while let Some(msg) = ws.next().await {
            match msg {
                Ok(tokio_tungstenite::tungstenite::Message::Text(t)) => {
                    text = Some(t.to_string());
                    break;
                }
                Ok(_) => continue,
                Err(e) => {
                    log::warn!("readiness probe {port}: socket error waiting for reply: {e}");
                    break;
                }
            }
        }
        text
    })
    .await
    .ok()
    .flatten();
    let _ = ws.close(None).await;
    match &reply {
        Some(t) if t.contains("moonshine-v2") => ProbeOutcome::Ready,
        Some(t) => {
            // Reached the server but it did not vouch for itself: either a
            // different process owns the port, or a stale sidecar build is
            // running. Neither will ever answer correctly, so stop retrying.
            log::warn!(
                "readiness probe {port}: unexpected reply {t:?} - port is held by another process"
            );
            ProbeOutcome::Incompatible
        }
        None => {
            log::warn!(
                "readiness probe {port}: connected but no reply within 2s - the sidecar on this \
                 port is stale (built before the auth-token protocol) or not a moonshine server"
            );
            ProbeOutcome::Incompatible
        }
    }
}

/// Client wire contract for the local ASR sidecars:
/// [optional 32-hex auth token (moonshine sidecar only; must match
/// moonshine-v2-server::TOKEN_LEN)][u32LE sample_rate][u32LE byte_len]
/// [f32 LE samples...]. Frame-building only — sample bytes pass through
/// untouched (audio/DSP freeze respected).
fn build_audio_payload(samples: &[f32], auth_token: Option<&str>, sample_rate: u32) -> Vec<u8> {
    let token_len = auth_token.map(str::len).unwrap_or(0);
    let byte_len = (samples.len() * 4) as u32;
    let mut payload = Vec::with_capacity(token_len + 8 + samples.len() * 4);
    if let Some(token) = auth_token {
        payload.extend_from_slice(token.as_bytes());
    }
    payload.extend_from_slice(&sample_rate.to_le_bytes());
    payload.extend_from_slice(&byte_len.to_le_bytes());
    for &sample in samples {
        payload.extend_from_slice(&sample.to_le_bytes());
    }
    payload
}

/// Path we record our sidecar child's PID in, so a later run can reap it.
fn sidecar_pid_file() -> Option<std::path::PathBuf> {
    let dir = dirs::data_local_dir()?.join("Fluence");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("sidecar.pid"))
}

/// Remember the PID of a sidecar we just spawned.
fn record_sidecar_pid(child: &Child) {
    let Some(path) = sidecar_pid_file() else {
        return;
    };
    let _ = std::fs::write(path, child.id().to_string());
}

pub fn forget_sidecar_pid() {
    if let Some(path) = sidecar_pid_file() {
        let _ = std::fs::remove_file(path);
    }
}

/// Kill a sidecar left behind by a previous run of the app.
///
/// The app is single-instance, so a PID we recorded earlier and that is still
/// alive belongs to an orphan. Windows does not run destructors when the app
/// is terminated, so these otherwise accumulate and squat the fixed port range
/// (6006..=6029). A squatter answers the readiness probe's TCP connect but
/// ignores the `probe:<token>` frame, which is indistinguishable from an
/// out-of-date sidecar and previously surfaced as a misleading "sidecar is
/// outdated" error.
///
/// Only the exact PID we recorded is ever signalled, so an unrelated process
/// that happens to reuse the ID after ours exited is not touched (we verify
/// the executable name before terminating).
pub fn reap_orphaned_sidecar() {
    let Some(path) = sidecar_pid_file() else {
        return;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(pid) = text.trim().parse::<u32>() else {
        let _ = std::fs::remove_file(&path);
        return;
    };
    if kill_pid_if_sidecar(pid) {
        log::warn!("reaped orphaned local-ASR sidecar (pid {pid}) from a previous run");
    }
    let _ = std::fs::remove_file(path);
}

#[cfg(target_os = "windows")]
fn kill_pid_if_sidecar(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, TerminateProcess, PROCESS_NAME_FORMAT,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    };
    const SIDECAR_NAMES: [&str; 2] = [
        "moonshine-v2-server",
        "sherpa-onnx-offline-websocket-server",
    ];

    // SAFETY: every handle is closed on every path; pid comes from our own file.
    unsafe {
        let Ok(handle) = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
            false,
            pid,
        ) else {
            return false;
        };

        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let matched = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok()
            && {
                let path = String::from_utf16_lossy(&buf[..len as usize]);
                let file = path
                    .rsplit(['\\', '/'])
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches(".exe")
                    .to_ascii_lowercase();
                SIDECAR_NAMES.contains(&file.as_str())
            };

        let killed = matched && TerminateProcess(handle, 1).is_ok();
        let _ = CloseHandle(handle);
        killed
    }
}

#[cfg(target_os = "linux")]
fn kill_pid_if_sidecar(pid: u32) -> bool {
    const SIDECAR_NAMES: [&str; 2] = [
        "moonshine-v2-server",
        "sherpa-onnx-offline-websocket-server",
    ];

    // PIDs are recycled: only signal when the executable behind the
    // recorded PID is still one of our sidecars.
    let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) else {
        return false;
    };
    let name = exe
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !SIDECAR_NAMES.contains(&name.as_str()) {
        return false;
    }
    // SAFETY: `pid` comes from our own pid file and the executable behind
    // it was verified above. SIGTERM: the sidecars have no handler, so the
    // default disposition terminates them.
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) == 0 }
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn kill_pid_if_sidecar(_pid: u32) -> bool {
    false
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
                // Different engine requested - kill the current server
                log::info!(
                    "Switching engine from {} to {}, killing current server",
                    instance.engine.display_name(),
                    engine.display_name()
                );
                let _ = instance.child.kill();
                forget_sidecar_pid();
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
    // build tree - never in the model dir (see resolve_v2_sidecar). The
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

    if !exe_path.exists()
        || !runtime_dir
            .join(crate::offline_downloader::MOONSHINE_V2_ORT_DLL)
            .exists()
    {
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
    // We have no live sidecar and are about to take a port in the fixed
    // range, so any process recorded by a PREVIOUS run of the app is an
    // orphan squatting it. Reaped here rather than at startup: at startup a
    // double-launch (shortcut twice, autostart plus manual) would run the
    // reaper before the single-instance plugin had handed off to the running
    // instance, terminating that instance's live sidecar mid-transcription.
    // Reaching this point proves we ARE the running instance.
    reap_orphaned_sidecar();

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
    // Populated by the moonshine branch below; None for the sherpa sidecar.
    let mut spawn_token: Option<String> = None;

    let mut cmd = Command::new(&exe_path);
    cmd.current_dir(&offline_dir);
    #[cfg(target_os = "windows")]
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
            // Served by our own sidecar: same WS protocol family as the
            // sherpa server, model dir + streaming arch as CLI args. The
            // Moonshine core manages its own threading, so no
            // --num-threads is passed.
            let arch = engine.v2_arch().unwrap_or("small");
            let model_dir_arg = format!("--model-dir={}", offline_dir.display());
            let arch_arg = format!("--arch={arch}");
            // Fresh shared secret per spawn, delivered over stdin below —
            // NEVER on the command line, which same-user processes can read
            // via Win32_Process. Keeps other local processes from consuming
            // transcripts. No token value is ever logged.
            spawn_token = Some(generate_spawn_token());
            cmd.args([&model_dir_arg, &arch_arg, &port_arg]);
        }
    }
    // Stdin is only piped when a token needs to be delivered; the Sherpa
    // sidecar keeps its default (never reads stdin).
    if spawn_token.is_some() {
        cmd.stdin(Stdio::piped());
    }

    {
        let mut lock = SERVER_INSTANCE.lock().unwrap();
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow!("Failed to spawn ASR server: {}", e))?;
        // Private handshake: pipe the fresh token as ONE newline-terminated
        // line, then close stdin. The sidecar reads it with `read_line` and
        // trims CR/LF, so the terminator is the actual protocol — do not rely
        // on the pipe reaching EOF to complete the line. If the pipe fails,
        // kill the sidecar and fail the start instead of entering a
        // half-secret state.
        if let (Some(token), Some(mut stdin)) = (spawn_token.as_deref(), child.stdin.take()) {
            if let Err(e) = stdin
                .write_all(format!("{token}\n").as_bytes())
                .and_then(|_| stdin.flush())
            {
                log::error!("Failed to pipe auth token to ASR sidecar: {}", e);
                let _ = child.kill();
                return Err(anyhow!("Failed to deliver auth token to the ASR sidecar"));
            }
        }
        record_sidecar_pid(&child);
        *lock = Some(ServerInstance {
            child,
            port,
            engine,
            auth_token: spawn_token.clone(),
        });
        *LAST_USED.lock().unwrap() = Instant::now();
    }

    start_idle_monitor();

    // Readiness: a bare TCP connect alone is NOT proof that the spawned
    // sidecar owns the port; a squatter (or racing process) could hold the
    // port first and pass a naive check while the real sidecar dies on
    // bind. For the moonshine sidecar the readiness signal is therefore the
    // WebSocket handshake + `probe:<token>` reply naming this server.
    // Sherpa keeps the legacy bare-TCP contract (documented residual risk).
    //
    // The moonshine path must call the probe DIRECTLY. Gating it behind a
    // bare `std::net::TcpStream::connect` connected and immediately dropped a
    // socket on every poll: the sidecar accepted it, blocked on an HTTP
    // upgrade request that never came, and logged a spurious
    // "websocket accept failed: Handshake not finished" per attempt while
    // each dead connection also burned one of its MAX_CONCURRENT slots.
    // Budget readiness by deadline, not iteration count. The sidecar reads its
    // stdin token (up to 10s) and loads the ONNX model BEFORE it binds the
    // port, so a cold start takes far longer than the old fixed 30x100ms
    // (~3s) loop allowed - which killed a sidecar that was still loading and
    // surfaced as "nothing was pasted" with no error on the sidecar side.
    const READY_DEADLINE: Duration = Duration::from_secs(60);
    let give_up_at = Instant::now() + READY_DEADLINE;
    let mut ready = false;
    let mut incompatible = false;
    loop {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let ready_now = match spawn_token.as_deref() {
            Some(token) => match probe_moonshine_ready(port, token).await {
                ProbeOutcome::Ready => true,
                // Nothing listening yet - normal while the model loads.
                ProbeOutcome::NotYet => false,
                // A stale sidecar build will never answer. Retrying for the
                // full deadline only produced a minute of silent pulsing, so
                // stop and say why.
                ProbeOutcome::Incompatible => {
                    incompatible = true;
                    break;
                }
            },
            // Sherpa only: legacy bare-TCP readiness contract, unchanged.
            None => tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok(),
        };
        if ready_now {
            ready = true;
            break;
        }
        if Instant::now() >= give_up_at {
            log::error!(
                "{} server was not ready within {:?} on port {}",
                server_label,
                READY_DEADLINE,
                port
            );
            break;
        }
    }

    if !ready {
        let mut lock = SERVER_INSTANCE.lock().unwrap();
        if let Some(mut instance) = lock.take() {
            let _ = instance.child.kill();
            forget_sidecar_pid();
        }
        if incompatible {
            return Err(anyhow!(
                "The local transcription sidecar on port {port} answered a connection but not \
                 the start-up handshake. Either an orphaned sidecar from a previous run still \
                 holds the port, or the sidecar binary predates the auth-token protocol and \
                 needs rebuilding: cargo build --release -p moonshine-v2-server"
            ));
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

    // Copy the spawn-shared auth token for this engine instance (None for
    // sherpa). Guarded by the same engine check the server lookup implies.
    let auth_token = {
        let lock = SERVER_INSTANCE.lock().unwrap();
        lock.as_ref()
            .filter(|i| i.engine == engine)
            .and_then(|i| i.auth_token.clone())
    };

    let url = format!("ws://127.0.0.1:{}", port);
    let (mut ws_stream, _) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async(&url),
    )
    .await
    .map_err(|_| anyhow!("Timed out connecting to local ASR server"))?
    .map_err(|e| {
        // The sidecar answers overloaded connects with a real HTTP 503
        // ("at capacity"). Surface that as a distinct, recoverable error
        // instead of a generic transport failure.
        if e.to_string().contains("503") {
            anyhow!(
                "Local ASR server is busy (connection refused at the sidecar's capacity limit); short wait and try again"
            )
        } else {
            anyhow!("Failed to connect to local ASR server: {}", e)
        }
    })?;

    // Build the sidecar binary payload:
    // [optional 32-hex auth token][int32LE sample_rate][int32LE num_audio_bytes][f32 samples...]
    let sample_rate = 16000u32;
    let payload = build_audio_payload(samples, auth_token.as_deref(), sample_rate);

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
    // Do NOT send "Done" before receiving the result - it closes the connection.

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

    /// Shape-valid but meaningless token for negative-path tests.
    const TEST_TOKEN: &str = "00000000000000000000000000000000";

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
        let exe = crate::offline_downloader::MOONSHINE_V2_SERVER_EXE;
        assert_eq!(
            names,
            vec![
                format!("C:/install/{exe}"),
                format!("D:/repo/src-tauri/target/release/{exe}"),
                format!("D:/repo/src-tauri/target/debug/{exe}"),
                format!(
                    "D:/repo/src-tauri/binaries/moonshine-v2-server-{V2_SIDECAR_TRIPLE}{V2_SIDECAR_BIN_SUFFIX}"
                ),
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

    #[test]
    fn v2_sidecar_dll_candidates_cover_install_and_dev_layouts() {
        use std::path::PathBuf;
        let exe = PathBuf::from(format!(
            "C:/install/{}",
            crate::offline_downloader::MOONSHINE_V2_SERVER_EXE
        ));
        let names: Vec<String> = v2_sidecar_dll_candidates(&exe)
            .iter()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .collect();
        // Production NSIS layout splits the pair (exe at root, dll under
        // binaries/), so both spots must be accepted or every installed
        // build fails resolution with a "sibling missing" error.
        let dll = crate::offline_downloader::MOONSHINE_V2_ORT_DLL;
        assert_eq!(
            names,
            vec![
                format!("C:/install/{dll}"),
                format!("C:/install/binaries/{dll}"),
            ]
        );
    }

    // ── Sidecar wire contracts ─────────────────────────────────────────
    // NOTE: these pin the parent half of the shared protocol with
    // moonshine-v2-server (auth token, payload framing, size limits).
    // The sidecar's own behavioral tests live in that crate; passing the
    // unrelated suites here is NOT a claim of end-to-end sidecar coverage.

    #[test]
    fn spawn_token_contract_is_32_hex_chars() {
        let token = generate_spawn_token();
        assert_eq!(token.len(), 32);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn audio_payload_with_token_matches_sidecar_contract() {
        let token = generate_spawn_token();
        let samples = [0.5f32, -0.25];
        let payload = build_audio_payload(&samples, Some(&token), 16000);
        assert_eq!(payload.len(), 32 + 8 + 8);
        assert_eq!(&payload[0..32], token.as_bytes());
        let head = &payload[32..40];
        assert_eq!(&head[0..4], &16000u32.to_le_bytes());
        assert_eq!(&head[4..8], &8u32.to_le_bytes());
        assert_eq!(&payload[40..44], &0.5f32.to_le_bytes());
        assert_eq!(&payload[44..48], &(-0.25f32).to_le_bytes());
    }

    #[test]
    fn audio_payload_without_token_keeps_the_bare_sherpa_header() {
        let payload = build_audio_payload(&[1.0f32], None, 16000);
        assert_eq!(payload.len(), 12);
        assert_eq!(&payload[0..4], &16000u32.to_le_bytes());
        assert_eq!(&payload[4..8], &4u32.to_le_bytes());
        assert_eq!(&payload[8..12], &1.0f32.to_le_bytes());
    }

    #[test]
    fn max_recording_message_fits_the_sidecar_socket_bound() {
        // 32-byte token + 8-byte header + the app's maximum utterance
        // (10 min @ 16 kHz f32 = 38.4 MB) must stay within tungstenite's
        // 64 MiB default message limit — the sidecar ships with those
        // defaults, so this pin prevents a silent recording-limit cut.
        const SIDE_MACHINE_DEFAULT_MAX_MESSAGE: usize = 64 * 1024 * 1024;
        assert!(
            32 + 8 + crate::transcribe::MAX_OFFLINE_SAMPLES * 4 <= SIDE_MACHINE_DEFAULT_MAX_MESSAGE
        );
    }

    // The probe must fail closed - return false, not hang, panic, or treat an
    // unanswered port as ready - when nothing is listening. Uses a port this
    // test does not bind, so it needs no sidecar and no external process.
    //
    // The positive path (probe against a live sidecar) is deliberately NOT
    // asserted here: it requires a spawned moonshine-v2-server with a matching
    // token and is covered by the manual E2E pass, not by `cargo test`.
    #[tokio::test]
    async fn readiness_probe_fails_closed_when_nothing_is_listening() {
        for port in [6399u16, 6398] {
            let ok = probe_moonshine_ready(port, TEST_TOKEN).await == ProbeOutcome::Ready;
            assert!(!ok, "probe must report not-ready on dead port {port}");
        }
    }

    // End-to-end probe against a REAL spawned sidecar: spawn, deliver the
    // token over stdin, wait for the model to load, then require the
    // authenticated probe to succeed.
    //
    // `#[ignore]`d on purpose. It needs a downloaded model (~GBs) and spends
    // ~10-60s loading ONNX weights, so it cannot run in `cargo test` or in
    // CI. Run it by hand when the sidecar protocol changes:
    //
    //     cargo test -p fluence-windows --lib -- --ignored --nocapture
    //
    // It self-skips (passes) when the sidecar or model is absent, so it is
    // safe to run on a machine without them.
    #[tokio::test]
    #[ignore = "requires a downloaded moonshine_v2 model; run manually"]
    async fn live_sidecar_completes_the_stdin_token_and_probe_handshake() {
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let target = manifest.join("target");
        let Some(exe) = v2_sidecar_candidates(None, &manifest, &target)
            .into_iter()
            .find(|p| p.is_file())
        else {
            eprintln!("SKIP: no staged moonshine-v2-server.exe");
            return;
        };
        let model_dir = crate::offline_downloader::get_moonshine_v2_small_dir();
        if !model_dir.join("encoder.ort").is_file() {
            eprintln!(
                "SKIP: no moonshine_v2_small model at {}",
                model_dir.display()
            );
            return;
        }

        // Pick a port nothing is using.
        let mut port = 6231u16;
        while port < 6400 && !is_port_available(port) {
            port += 1;
        }
        assert!(port < 6400, "no free port for the live sidecar test");

        let token = generate_spawn_token();
        let mut child = Command::new(&exe)
            .arg(format!("--port={port}"))
            .arg(format!("--model-dir={}", model_dir.display()))
            .arg("--arch=small")
            .current_dir(&model_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sidecar");

        // Private handshake: one newline-terminated line, then close stdin.
        {
            let mut stdin = child.stdin.take().expect("piped stdin");
            use std::io::Write as _;
            stdin
                .write_all(format!("{token}\n").as_bytes())
                .and_then(|_| stdin.flush())
                .expect("write token to sidecar stdin");
        }

        // The sidecar binds only AFTER loading the model, so allow a long
        // wait here even though the app's own budget is 60s.
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut ok = false;
        while Instant::now() < deadline {
            if probe_moonshine_ready(port, &token).await == ProbeOutcome::Ready {
                ok = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let _ = child.kill();
        let _ = child.wait();

        assert!(
            ok,
            "live sidecar never answered the authenticated probe on port {port} \
             (check the sidecar actually rebuilt and supports probe:/stdin tokens)"
        );
    }

    #[tokio::test]
    async fn oversized_recording_fails_fast_with_a_clear_recoverable_error() {
        let samples = vec![0.0f32; crate::transcribe::MAX_OFFLINE_SAMPLES + 1];
        let err = transcribe_samples(&samples, OfflineEngine::MoonshineV2Small)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Recording too long"),
            "unexpected error: {msg}"
        );
        assert!(
            msg.contains("Maximum is 10 minutes"),
            "duration limit must be reported in user units: {msg}"
        );
    }
}
