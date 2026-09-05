// Fluence Windows — Offline Asset Downloader
// Downloads and extracts sherpa-onnx binaries and SenseVoice model files.

use anyhow::{anyhow, Context, Result};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;

// Global cancellation flag
static DOWNLOAD_CANCELLED: Lazy<AtomicBool> = Lazy::new(|| AtomicBool::new(false));
// Global downloading flag to prevent concurrent downloads
static IS_DOWNLOADING: Lazy<AtomicBool> = Lazy::new(|| AtomicBool::new(false));

// ── Sherpa manifest ──────────────────────────────────────────────
// Pinned release version, download URLs, and expected SHA-256 hashes.
// Update this file (src-tauri/sherpa-manifest.json) when bumping Sherpa.

#[derive(Deserialize)]
pub(crate) struct ManifestDownload {
    pub url: String,
    pub filename: String,
    pub sha256: String,
}

#[derive(Deserialize)]
pub(crate) struct SherpaManifest {
    #[allow(dead_code)]
    pub sherpa_version: String,
    pub downloads: Vec<ManifestDownload>,
    pub expected_binaries: HashMap<String, String>,
}

pub(crate) static MANIFEST: Lazy<SherpaManifest> = Lazy::new(|| {
    serde_json::from_str(include_str!("../sherpa-manifest.json"))
        .expect("Failed to parse sherpa-manifest.json — this file must exist at src-tauri/sherpa-manifest.json")
});

/// Look up a download entry by filename. Fails fast if missing.
pub(crate) fn manifest_download(filename: &str) -> Result<&'static ManifestDownload> {
    MANIFEST
        .downloads
        .iter()
        .find(|d| d.filename == filename)
        .with_context(|| format!("Missing manifest download entry for '{}'", filename))
}

/// Look up expected hash for an extracted binary. Fails fast if missing.
pub(crate) fn manifest_binary_hash(name: &str) -> Result<&'static str> {
    MANIFEST
        .expected_binaries
        .get(name)
        .map(|s| s.as_str())
        .with_context(|| format!("Missing expected binary hash for '{}'", name))
}

// ── Moonshine v2 streaming manifests ─────────────────────────────
// Same model files as the Android app (download.moonshine.ai, 8 per-file
// downloads, no archive). Per-file SHA-256 pins are baked into the
// *-manifest.json files; any mismatch fails verification loudly rather
// than silently trusting bytes.
#[derive(Deserialize)]
pub(crate) struct MoonshineV2File {
    pub name: String,
    pub sha256: String,
    /// Pinned size in bytes: doubles as the exact-match sanity gate (see
    /// perform/download + readiness check), so tiny-but-valid files like
    /// streaming_config.json (~512 bytes) are accepted.
    pub bytes: u64,
}

#[derive(Deserialize)]
pub(crate) struct MoonshineV2Manifest {
    #[allow(dead_code)]
    pub model_name: String,
    pub arch: String,
    pub base_url: String,
    pub total_bytes: u64,
    pub server_exe_sha256: Option<String>,
    pub files: Vec<MoonshineV2File>,
}

pub(crate) static MOONSHINE_V2_SMALL_MANIFEST: Lazy<MoonshineV2Manifest> = Lazy::new(|| {
    serde_json::from_str(include_str!("../moonshine-v2-small-manifest.json"))
        .expect("Failed to parse moonshine-v2-small-manifest.json")
});

pub(crate) static MOONSHINE_V2_MEDIUM_MANIFEST: Lazy<MoonshineV2Manifest> = Lazy::new(|| {
    serde_json::from_str(include_str!("../moonshine-v2-medium-manifest.json"))
        .expect("Failed to parse moonshine-v2-medium-manifest.json")
});

/// Look up the v2 manifest by architecture id ("small" | "medium") without
/// coupling this module to `OfflineEngine`.
pub(crate) fn moonshine_v2_manifest_for_arch(arch: &str) -> Result<&'static MoonshineV2Manifest> {
    match arch {
        "small" => Ok(&MOONSHINE_V2_SMALL_MANIFEST),
        "medium" => Ok(&MOONSHINE_V2_MEDIUM_MANIFEST),
        _ => Err(anyhow!("Unknown Moonshine v2 architecture: {}", arch)),
    }
}

/// Verify SHA-256 hash of a file against expected hex digest.
pub fn verify_sha256(path: &Path, expected_hex: &str) -> Result<()> {
    let data = fs::read(path)
        .with_context(|| format!("Failed to read {} for integrity check", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    let actual_hex = hex::encode(hasher.finalize());
    if actual_hex != expected_hex {
        return Err(anyhow!(
            "Integrity check failed for {}: expected {}…, got {}…",
            path.display(),
            &expected_hex[..16.min(expected_hex.len())],
            &actual_hex[..16]
        ));
    }
    Ok(())
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadProgressPayload {
    status: String, // "idle" | "downloading" | "extracting" | "completed" | "error" | "cancelled"
    progress: f64,
    bytes_downloaded: u64,
    total_bytes: u64,
    current_file: String,
    error_message: Option<String>,
}

pub fn get_offline_dir() -> PathBuf {
    let mut path = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("Fluence");
    path.push("bin");
    path.push("sensevoice_v2");
    path
}

pub fn check_model_files_exist() -> bool {
    let dir = get_offline_dir();
    let model_file = dir.join("model.int8.onnx");
    let tokens_file = dir.join("tokens.txt");
    let binary_file = dir.join("sherpa-onnx-offline-websocket-server.exe");
    let dll_file = dir.join("onnxruntime.dll");

    model_file.exists()
        && tokens_file.exists()
        && binary_file.exists()
        && dll_file.exists()
        && model_file.metadata().map(|m| m.len()).unwrap_or(0) > 100_000_000
}

fn emit_progress(app: Option<&AppHandle>, payload: DownloadProgressPayload) {
    if let Some(app) = app {
        let _ = app.emit("offline-download-progress", payload);
    }
}

pub fn cancel_download() {
    if IS_DOWNLOADING.load(Ordering::SeqCst) {
        DOWNLOAD_CANCELLED.store(true, Ordering::SeqCst);
    }
}

pub fn delete_model_files() -> Result<u64> {
    let dir = get_offline_dir();
    if !dir.exists() {
        return Ok(0);
    }

    let mut bytes_freed = 0;
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if let Ok(meta) = entry.metadata() {
            bytes_freed += meta.len();
        }
        if path.is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
    }

    // Remove directory itself
    fs::remove_dir(dir)?;
    Ok(bytes_freed)
}

pub async fn start_download_task(app: AppHandle) -> Result<()> {
    if IS_DOWNLOADING.load(Ordering::SeqCst) {
        return Err(anyhow!("Download is already in progress"));
    }

    IS_DOWNLOADING.store(true, Ordering::SeqCst);
    DOWNLOAD_CANCELLED.store(false, Ordering::SeqCst);

    let app_clone = app.clone();
    tokio::spawn(async move {
        match perform_download(Some(&app_clone)).await {
            Ok(_) => {
                IS_DOWNLOADING.store(false, Ordering::SeqCst);
                emit_progress(
                    Some(&app_clone),
                    DownloadProgressPayload {
                        status: "completed".to_string(),
                        progress: 100.0,
                        bytes_downloaded: 0,
                        total_bytes: 0,
                        current_file: "".to_string(),
                        error_message: None,
                    },
                );
            }
            Err(e) => {
                IS_DOWNLOADING.store(false, Ordering::SeqCst);
                let err_msg = e.to_string();
                log::error!("Offline download failed: {}", err_msg);

                let status = if DOWNLOAD_CANCELLED.load(Ordering::SeqCst) {
                    "cancelled".to_string()
                } else {
                    "error".to_string()
                };

                emit_progress(
                    Some(&app_clone),
                    DownloadProgressPayload {
                        status,
                        progress: 0.0,
                        bytes_downloaded: 0,
                        total_bytes: 0,
                        current_file: "".to_string(),
                        error_message: Some(err_msg),
                    },
                );

                // Clean up incomplete temp files
                let dir = get_offline_dir();
                let _ = clean_temp_files(&dir);
            }
        }
    });

    Ok(())
}

async fn perform_download(app: Option<&AppHandle>) -> Result<()> {
    let dest_dir = get_offline_dir();
    fs::create_dir_all(&dest_dir)?;

    // We estimate the sizes:
    // sherpa-onnx archive: ~24 MB
    // model: ~239 MB
    // tokens: ~300 KB
    // Total: ~263 MB
    let total_bytes: u64 = 263_500_000;
    let mut bytes_downloaded: u64 = 0;

    // Use the shared HTTP client (reuses the global connection pool)
    let client = &crate::http_client::CLIENT;

    ensure_server_runtime(&client, &dest_dir, &mut bytes_downloaded, total_bytes, app).await?;

    // 3. Download SenseVoice model.int8.onnx
    let model_info = manifest_download("model.int8.onnx")?;
    let model_path = dest_dir.join("model.int8.onnx");
    download_file_to_path(
        &client,
        &model_info.url,
        &model_path,
        "SenseVoice model",
        &mut bytes_downloaded,
        total_bytes,
        app,
    )
    .await?;
    verify_sha256_and_remove(&model_path, &model_info.sha256)?;

    // 4. Download SenseVoice tokens.txt
    let tokens_info = manifest_download("tokens.txt")?;
    let tokens_path = dest_dir.join("tokens.txt");
    download_file_to_path(
        &client,
        &tokens_info.url,
        &tokens_path,
        "SenseVoice tokens",
        &mut bytes_downloaded,
        total_bytes,
        app,
    )
    .await?;
    verify_sha256_and_remove(&tokens_path, &tokens_info.sha256)?;

    Ok(())
}

fn verify_sha256_and_remove(path: &Path, expected_hex: &str) -> Result<()> {
    match verify_sha256(path, expected_hex) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(path);
            Err(error)
        }
    }
}

async fn ensure_server_runtime(
    client: &reqwest::Client,
    dest_dir: &Path,
    bytes_downloaded: &mut u64,
    total_bytes: u64,
    app: Option<&AppHandle>,
) -> Result<()> {
    let exe_path = dest_dir.join("sherpa-onnx-offline-websocket-server.exe");
    let dll_path = dest_dir.join("onnxruntime.dll");
    let exe_hash = manifest_binary_hash("sherpa-onnx-offline-websocket-server.exe")?;
    if exe_path.exists() && dll_path.exists() && verify_sha256(&exe_path, exe_hash).is_ok() {
        return Ok(());
    }

    let archive_info = manifest_download("sherpa-onnx-win-x64.tar.bz2")?;
    let archive_path = dest_dir.join("sherpa-onnx-win-x64.tar.bz2");
    download_file_to_path(
        client,
        &archive_info.url,
        &archive_path,
        "sherpa-onnx binaries",
        bytes_downloaded,
        total_bytes,
        app,
    )
    .await?;
    verify_sha256_and_remove(&archive_path, &archive_info.sha256)?;

    let temp_extract_dir = dest_dir.join("temp_extract");
    fs::create_dir_all(&temp_extract_dir)?;
    emit_progress(
        app,
        DownloadProgressPayload {
            status: "extracting".to_string(),
            progress: (*bytes_downloaded as f64 / total_bytes as f64 * 100.0).min(99.0),
            bytes_downloaded: *bytes_downloaded,
            total_bytes,
            current_file: "extracting binaries".to_string(),
            error_message: None,
        },
    );

    let output = tokio::process::Command::new("tar")
        .arg("-xjf")
        .arg(&archive_path)
        .arg("-C")
        .arg(&temp_extract_dir)
        .output()
        .await
        .map_err(|e| {
            let _ = fs::remove_dir_all(&temp_extract_dir);
            let _ = fs::remove_file(&archive_path);
            anyhow!("Failed to run tar command: {}", e)
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = fs::remove_dir_all(&temp_extract_dir);
        let _ = fs::remove_file(&archive_path);
        return Err(anyhow!("Failed to extract archive: {}", stderr));
    }

    if let Err(error) = copy_extracted_files(&temp_extract_dir, dest_dir) {
        let _ = fs::remove_dir_all(&temp_extract_dir);
        let _ = fs::remove_file(&archive_path);
        return Err(error);
    }
    if let Err(error) = verify_sha256_and_remove(&exe_path, exe_hash) {
        let _ = fs::remove_dir_all(&temp_extract_dir);
        let _ = fs::remove_file(&archive_path);
        return Err(error);
    }
    if !dll_path.exists() {
        let _ = fs::remove_file(&exe_path);
        let _ = fs::remove_dir_all(&temp_extract_dir);
        let _ = fs::remove_file(&archive_path);
        return Err(anyhow!("Offline runtime is missing onnxruntime.dll"));
    }

    let _ = fs::remove_dir_all(&temp_extract_dir);
    let _ = fs::remove_file(&archive_path);
    Ok(())
}

async fn download_file_to_path(
    client: &reqwest::Client,
    url: &str,
    dest_path: &Path,
    file_label: &str,
    bytes_downloaded: &mut u64,
    total_bytes: u64,
    app: Option<&AppHandle>,
) -> Result<()> {
    if DOWNLOAD_CANCELLED.load(Ordering::SeqCst) {
        return Err(anyhow!("Download cancelled by user"));
    }

    log::info!("Downloading {} from {}", file_label, url);
    let tmp_path = dest_path.with_extension("tmp");
    // Large asset downloads (model ~239MB) need a generous per-request timeout
    // independent of the shared client's 30s API-response timeout.
    let mut response = client
        .get(url)
        .timeout(std::time::Duration::from_secs(600))
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(anyhow!(
            "Download failed with HTTP status: {}",
            response.status()
        ));
    }

    let mut file = tokio::fs::File::create(&tmp_path).await?;

    while let Some(chunk) = response.chunk().await? {
        if DOWNLOAD_CANCELLED.load(Ordering::SeqCst) {
            let _ = file.shutdown().await;
            drop(file);
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(anyhow!("Download cancelled by user"));
        }

        file.write_all(&chunk).await?;
        *bytes_downloaded += chunk.len() as u64;

        let progress = (*bytes_downloaded as f64 / total_bytes as f64 * 100.0).min(99.0);
        emit_progress(
            app,
            DownloadProgressPayload {
                status: "downloading".to_string(),
                progress,
                bytes_downloaded: *bytes_downloaded,
                total_bytes,
                current_file: file_label.to_string(),
                error_message: None,
            },
        );
    }

    file.shutdown().await?;
    drop(file);

    // Atomic rename to target file
    if dest_path.exists() {
        let _ = fs::remove_file(dest_path);
    }
    fs::rename(tmp_path, dest_path)?;

    Ok(())
}

fn copy_extracted_files(src_dir: &Path, dest_dir: &Path) -> Result<()> {
    // Traverse the directory recursively to find sherpa-onnx-offline-websocket-server.exe and *.dll files
    fn traverse_and_copy(dir: &Path, dest: &Path) -> Result<()> {
        if dir.is_dir() {
            for entry in fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_dir() {
                    traverse_and_copy(&path, dest)?;
                } else if path.is_file() {
                    let file_name = path.file_name().unwrap().to_string_lossy();
                    if file_name.ends_with(".dll")
                        || file_name == "sherpa-onnx-offline-websocket-server.exe"
                    {
                        let dest_file = dest.join(&*file_name);
                        log::info!("Copying helper file: {:?} -> {:?}", path, dest_file);
                        fs::copy(&path, &dest_file)?;
                    }
                }
            }
        }
        Ok(())
    }

    traverse_and_copy(src_dir, dest_dir)
}

fn clean_temp_files(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        if path.is_file() {
            let name = path.file_name().unwrap().to_string_lossy();
            if name.ends_with(".tmp") || name.ends_with(".tar.bz2") {
                let _ = fs::remove_file(path);
            }
        } else if path.is_dir() && path.file_name().unwrap().to_string_lossy() == "temp_extract" {
            let _ = fs::remove_dir_all(path);
        }
    }
    Ok(())
}

// Tauri Command wrappers
#[tauri::command]
pub async fn download_offline_model(app: tauri::AppHandle) -> Result<(), String> {
    start_download_task(app).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_offline_model_status() -> bool {
    check_model_files_exist()
}

#[tauri::command]
pub fn cancel_offline_download() {
    cancel_download()
}

#[tauri::command]
pub fn delete_offline_model() -> Result<u64, String> {
    delete_model_files().map_err(|e| e.to_string())
}

// ── Moonshine v2 streaming (small/medium) ────────────────────────
// Clone of the Moonshine v1 flow, but per-file downloads (no archive).
// The sidecar runtime (moonshine-v2-server.exe + onnxruntime.dll) SHIPS
// WITH THE APP INSTALLER (Tauri externalBin) or a dev build tree — it is
// deliberately NOT part of the model download and NOT expected in the
// model dir. Model readiness therefore covers the 8 model files only;
// runtime resolution + hash gating happen at spawn time in
// offline_transcribe::resolve_v2_sidecar.

/// Sidecar runtime binary served by our own Moonshine v2 server (built
/// from the official moonshine-ai/moonshine C++ core).
pub const MOONSHINE_V2_SERVER_EXE: &str = "moonshine-v2-server.exe";
pub const MOONSHINE_V2_ORT_DLL: &str = "onnxruntime.dll";

pub fn get_moonshine_v2_small_dir() -> PathBuf {
    let mut path = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("Fluence");
    path.push("bin");
    path.push("moonshine_v2_small");
    path
}

pub fn get_moonshine_v2_medium_dir() -> PathBuf {
    let mut path = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("Fluence");
    path.push("bin");
    path.push("moonshine_v2_medium");
    path
}

fn check_moonshine_v2_files_exist(dir: &Path, manifest: &MoonshineV2Manifest) -> bool {
    if !dir.exists() {
        return false;
    }
    // Model files only, matched against pinned manifest sizes. No blanket
    // floor: streaming_config.json is legitimately ~512 bytes. The sidecar
    // runtime ships with the installer / dev build tree and is resolved at
    // spawn time, so requiring it here would make a completed download flip
    // back to "not installed".
    manifest.files.iter().all(|f| {
        let p = dir.join(&f.name);
        p.exists() && p.metadata().map(|m| m.len()).unwrap_or(0) == f.bytes
    })
}

pub fn check_moonshine_v2_small_files_exist() -> bool {
    check_moonshine_v2_files_exist(&get_moonshine_v2_small_dir(), &MOONSHINE_V2_SMALL_MANIFEST)
}

pub fn check_moonshine_v2_medium_files_exist() -> bool {
    check_moonshine_v2_files_exist(
        &get_moonshine_v2_medium_dir(),
        &MOONSHINE_V2_MEDIUM_MANIFEST,
    )
}

fn delete_moonshine_v2_files(dir: &Path) -> Result<u64> {
    if !dir.exists() {
        return Ok(0);
    }
    let mut bytes_freed = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if let Ok(meta) = entry.metadata() {
            bytes_freed += meta.len();
        }
        if path.is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
    }
    fs::remove_dir(dir)?;
    Ok(bytes_freed)
}

pub fn delete_moonshine_v2_small_files() -> Result<u64> {
    delete_moonshine_v2_files(&get_moonshine_v2_small_dir())
}

pub fn delete_moonshine_v2_medium_files() -> Result<u64> {
    delete_moonshine_v2_files(&get_moonshine_v2_medium_dir())
}

async fn perform_moonshine_v2_download(
    manifest: &MoonshineV2Manifest,
    dest_dir: &Path,
    label: &str,
    app: Option<&AppHandle>,
) -> Result<()> {
    fs::create_dir_all(dest_dir)?;
    let client = &crate::http_client::CLIENT;
    let mut bytes_downloaded: u64 = 0;

    for file in &manifest.files {
        let url = format!("{}{}", manifest.base_url, file.name);
        download_file_to_path(
            client,
            &url,
            &dest_dir.join(&file.name),
            &format!("{label} {}", file.name),
            &mut bytes_downloaded,
            manifest.total_bytes,
            app,
        )
        .await?;
        // Pinned hash: fails loudly on mismatch or corruption.
        verify_sha256(&dest_dir.join(&file.name), &file.sha256)?;
    }

    // Size sanity against each file's pinned manifest size. A blanket
    // floor is wrong here: tiny-but-valid files exist
    // (streaming_config.json is ~512 bytes) and already passed SHA-256
    // above, so anything but an exact match means corruption.
    for file in &manifest.files {
        let p = dest_dir.join(&file.name);
        let len = p.metadata()?.len();
        if len != file.bytes {
            for f in &manifest.files {
                let _ = fs::remove_file(dest_dir.join(&f.name));
            }
            return Err(anyhow!(
                "Model file '{}' has unexpected size ({} bytes, expected {}), likely corrupt",
                file.name,
                len,
                file.bytes
            ));
        }
    }

    log::info!("{label} model downloaded and verified successfully");
    Ok(())
}

fn emit_v2_download_result(
    app: &AppHandle,
    result: Result<()>,
    label: &str,
    dest_dir: &Path,
    manifest: &MoonshineV2Manifest,
) {
    IS_DOWNLOADING.store(false, Ordering::SeqCst);
    match result {
        Ok(_) => emit_progress(
            Some(app),
            DownloadProgressPayload {
                status: "completed".to_string(),
                progress: 100.0,
                bytes_downloaded: 0,
                total_bytes: 0,
                current_file: "".to_string(),
                error_message: None,
            },
        ),
        Err(e) => {
            let err_msg = e.to_string();
            log::error!("{label} download failed: {}", err_msg);
            let status = if DOWNLOAD_CANCELLED.load(Ordering::SeqCst) {
                "cancelled".to_string()
            } else {
                "error".to_string()
            };
            emit_progress(
                Some(app),
                DownloadProgressPayload {
                    status,
                    progress: 0.0,
                    bytes_downloaded: 0,
                    total_bytes: 0,
                    current_file: "".to_string(),
                    error_message: Some(err_msg),
                },
            );
            for f in &manifest.files {
                let _ = fs::remove_file(dest_dir.join(&f.name));
            }
            let _ = clean_temp_files(dest_dir);
        }
    }
}

pub async fn start_moonshine_v2_small_download_task(app: AppHandle) -> Result<()> {
    start_moonshine_v2_download_task_for(&MOONSHINE_V2_SMALL_MANIFEST, "Moonshine v2 Small", app)
        .await
}

pub async fn start_moonshine_v2_medium_download_task(app: AppHandle) -> Result<()> {
    start_moonshine_v2_download_task_for(&MOONSHINE_V2_MEDIUM_MANIFEST, "Moonshine v2 Medium", app)
        .await
}

async fn start_moonshine_v2_download_task_for(
    manifest: &'static MoonshineV2Manifest,
    label: &'static str,
    app: AppHandle,
) -> Result<()> {
    if IS_DOWNLOADING.load(Ordering::SeqCst) {
        return Err(anyhow!("Download is already in progress"));
    }
    IS_DOWNLOADING.store(true, Ordering::SeqCst);
    DOWNLOAD_CANCELLED.store(false, Ordering::SeqCst);

    let app_clone = app.clone();
    tokio::spawn(async move {
        let dest_dir = if manifest.arch == "small" {
            get_moonshine_v2_small_dir()
        } else {
            get_moonshine_v2_medium_dir()
        };
        let result =
            perform_moonshine_v2_download(manifest, &dest_dir, label, Some(&app_clone)).await;
        emit_v2_download_result(&app_clone, result, label, &dest_dir, manifest);
    });

    Ok(())
}

#[tauri::command]
pub async fn download_moonshine_v2_small_model(app: tauri::AppHandle) -> Result<(), String> {
    start_moonshine_v2_small_download_task(app)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_moonshine_v2_small_model_status() -> bool {
    check_moonshine_v2_small_files_exist()
}

#[tauri::command]
pub fn delete_moonshine_v2_small_model() -> Result<u64, String> {
    delete_moonshine_v2_small_files().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn download_moonshine_v2_medium_model(app: tauri::AppHandle) -> Result<(), String> {
    start_moonshine_v2_medium_download_task(app)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_moonshine_v2_medium_model_status() -> bool {
    check_moonshine_v2_medium_files_exist()
}

#[tauri::command]
pub fn delete_moonshine_v2_medium_model() -> Result<u64, String> {
    delete_moonshine_v2_medium_files().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::io::Write;

    #[test]
    fn manifest_loads_successfully() {
        let m = &*MANIFEST;
        assert!(!m.sherpa_version.is_empty());
        assert!(!m.downloads.is_empty());
        assert!(!m.expected_binaries.is_empty());
    }

    #[test]
    fn manifest_has_all_required_downloads() {
        let filenames: Vec<&str> = MANIFEST
            .downloads
            .iter()
            .map(|d| d.filename.as_str())
            .collect();
        assert!(filenames.contains(&"sherpa-onnx-win-x64.tar.bz2"));
        assert!(filenames.contains(&"model.int8.onnx"));
        assert!(filenames.contains(&"tokens.txt"));
    }

    #[test]
    fn manifest_download_lookup() {
        let dl = manifest_download("sherpa-onnx-win-x64.tar.bz2").unwrap();
        assert!(dl.url.starts_with("https://"));
        assert!(!dl.sha256.is_empty());
    }

    #[test]
    fn manifest_download_lookup_missing() {
        assert!(manifest_download("nonexistent.zip").is_err());
    }

    #[test]
    fn manifest_binary_hash_lookup() {
        let hash = manifest_binary_hash("sherpa-onnx-offline-websocket-server.exe").unwrap();
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn manifest_binary_hash_lookup_missing() {
        assert!(manifest_binary_hash("nonexistent.exe").is_err());
    }

    #[test]
    fn verify_sha256_correct_hash() {
        let dir = std::env::temp_dir().join("fluence_test_verify");
        let _ = fs::create_dir_all(&dir);
        let file_path = dir.join("test_input.bin");
        let content = b"hello world fluence security test";
        let mut f = fs::File::create(&file_path).unwrap();
        f.write_all(content).unwrap();
        f.flush().unwrap();

        let mut hasher = Sha256::new();
        hasher.update(content);
        let expected = hex::encode(hasher.finalize());

        assert!(verify_sha256(&file_path, &expected).is_ok());
        let _ = fs::remove_file(&file_path);
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn verify_sha256_wrong_hash() {
        let dir = std::env::temp_dir().join("fluence_test_verify_fail");
        let _ = fs::create_dir_all(&dir);
        let file_path = dir.join("test_input.bin");
        let mut f = fs::File::create(&file_path).unwrap();
        f.write_all(b"some data").unwrap();
        f.flush().unwrap();

        let wrong_hash = "0000000000000000000000000000000000000000000000000000000000000000";
        assert!(verify_sha256(&file_path, wrong_hash).is_err());
        let _ = fs::remove_file(&file_path);
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn verify_sha256_missing_file() {
        let fake = Path::new("C:\\nonexistent\\file\\that\\does\\not\\exist.bin");
        assert!(verify_sha256(fake, "abcd").is_err());
    }

    #[test]
    fn verify_sha256_empty_file() {
        let dir = std::env::temp_dir().join("fluence_test_verify_empty");
        let _ = fs::create_dir_all(&dir);
        let file_path = dir.join("empty.bin");
        let mut f = fs::File::create(&file_path).unwrap();
        f.flush().unwrap();

        let mut hasher = Sha256::new();
        hasher.update(b"");
        let expected = hex::encode(hasher.finalize());

        assert!(verify_sha256(&file_path, &expected).is_ok());
        let _ = fs::remove_file(&file_path);
        let _ = fs::remove_dir(&dir);
    }

    /// END-TO-END: delete the offline model directory, then run the FULL
    /// download + SHA-256 verification flow against the real network and the
    /// pinned manifest. After success, confirm files exist and re-verify hashes.
    #[tokio::test]
    async fn e2e_delete_and_redownload_with_integrity() {
        // 1. Delete any existing model files
        let _ = delete_model_files();
        assert!(
            !get_offline_dir().exists()
                || get_offline_dir()
                    .read_dir()
                    .map(|mut d| d.next().is_none())
                    .unwrap_or(true),
            "offline dir should be empty after delete"
        );

        // 2. Run the full download + verification flow (no GUI handle needed)
        let result = perform_download(None).await;
        assert!(
            result.is_ok(),
            "full download+verify failed: {:?}",
            result.err()
        );

        // 3. Confirm files exist and pass integrity checks
        assert!(
            check_model_files_exist(),
            "model files missing after download"
        );

        let exe_hash = manifest_binary_hash("sherpa-onnx-offline-websocket-server.exe").unwrap();
        assert!(verify_sha256(
            &get_offline_dir().join("sherpa-onnx-offline-websocket-server.exe"),
            exe_hash
        )
        .is_ok());

        let model_info = manifest_download("model.int8.onnx").unwrap();
        assert!(verify_sha256(
            &get_offline_dir().join("model.int8.onnx"),
            &model_info.sha256
        )
        .is_ok());

        let tokens_info = manifest_download("tokens.txt").unwrap();
        assert!(verify_sha256(&get_offline_dir().join("tokens.txt"), &tokens_info.sha256).is_ok());
    }

    #[test]
    fn v2_manifests_parse_with_android_file_inventory() {
        // Exact 8-file inventory per model, matching the Android app's
        // MoonshineV2ModelManager (same download.moonshine.ai source).
        let expected = [
            "encoder.ort",
            "decoder_kv.ort",
            "cross_kv.ort",
            "adapter.ort",
            "frontend.weights.ort",
            "frontend.model.ort",
            "tokenizer.bin",
            "streaming_config.json",
        ];
        for manifest in [
            &*MOONSHINE_V2_SMALL_MANIFEST,
            &*MOONSHINE_V2_MEDIUM_MANIFEST,
        ] {
            let names: Vec<&str> = manifest.files.iter().map(|f| f.name.as_str()).collect();
            assert_eq!(
                names, expected,
                "unexpected file list in {}",
                manifest.model_name
            );
            assert!(manifest
                .base_url
                .starts_with("https://download.moonshine.ai/"));
            assert!(manifest.total_bytes > 100_000_000);
            for file in &manifest.files {
                assert!(!file.sha256.is_empty(), "empty hash for {}", file.name);
                assert!(file.bytes > 0, "zero size for {}", file.name);
            }
        }
        assert_eq!(MOONSHINE_V2_SMALL_MANIFEST.arch, "small");
        assert_eq!(MOONSHINE_V2_MEDIUM_MANIFEST.arch, "medium");
        assert_eq!(MOONSHINE_V2_SMALL_MANIFEST.total_bytes, 142300974);
        assert_eq!(MOONSHINE_V2_MEDIUM_MANIFEST.total_bytes, 269141623);
    }

    #[test]
    fn v2_manifest_arch_lookup() {
        assert_eq!(
            moonshine_v2_manifest_for_arch("small").unwrap().arch,
            "small"
        );
        assert_eq!(
            moonshine_v2_manifest_for_arch("medium").unwrap().arch,
            "medium"
        );
        assert!(moonshine_v2_manifest_for_arch("tiny").is_err());
    }

    #[test]
    fn v2_file_check_covers_model_files_only() {
        // Hermetic: scratch dir, not the real model dirs (which may later
        // hold provisioned models on a dev machine).
        let scratch =
            std::env::temp_dir().join(format!("fluence-v2-check-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&scratch);
        fs::create_dir_all(&scratch).unwrap();
        let manifest = &*MOONSHINE_V2_SMALL_MANIFEST;

        // sparse_file writes one byte then extends to the pinned size
        // without allocating real blocks, so even the ~81 MB decoder is
        // instant on NTFS.
        let sparse_file = |name: &str, len: u64| {
            let path = scratch.join(name);
            fs::write(&path, [0u8]).unwrap();
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_len(len)
                .unwrap();
        };

        // Empty dir: not ready.
        assert!(!check_moonshine_v2_files_exist(&scratch, manifest));
        // Seven of eight model files at exact pinned sizes: not ready.
        for file in manifest.files.iter().take(7) {
            sparse_file(&file.name, file.bytes);
        }
        assert!(!check_moonshine_v2_files_exist(&scratch, manifest));
        // All eight at exact pinned sizes — WITHOUT any sidecar runtime
        // present, and including the 512-byte streaming_config.json: ready.
        // A completed download must report Installed.
        let last = &manifest.files[7];
        assert!(last.bytes < 1000, "test assumes a tiny-but-valid file");
        sparse_file(&last.name, last.bytes);
        assert!(check_moonshine_v2_files_exist(&scratch, manifest));
        // Wrong size on one file (truncation/corruption): not ready.
        sparse_file(&last.name, last.bytes - 1);
        assert!(!check_moonshine_v2_files_exist(&scratch, manifest));
        let _ = fs::remove_dir_all(&scratch);
    }
}
