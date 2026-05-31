// Fluence Windows — Offline Asset Downloader
// Downloads and extracts sherpa-onnx binaries and SenseVoice model files.

use anyhow::{anyhow, Result};
use once_cell::sync::Lazy;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;

// Global cancellation flag
static DOWNLOAD_CANCELLED: Lazy<AtomicBool> = Lazy::new(|| AtomicBool::new(false));
// Global downloading flag to prevent concurrent downloads
static IS_DOWNLOADING: Lazy<AtomicBool> = Lazy::new(|| AtomicBool::new(false));

const SHERPA_ONNX_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/v1.12.23/sherpa-onnx-v1.12.23-win-x64-shared.tar.bz2";
const SENSEVOICE_MODEL_URL: &str = "https://huggingface.co/csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17/resolve/main/model.int8.onnx";
const SENSEVOICE_TOKENS_URL: &str = "https://huggingface.co/csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17/resolve/main/tokens.txt";

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

fn emit_progress(app: &AppHandle, payload: DownloadProgressPayload) {
    let _ = app.emit("offline-download-progress", payload);
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
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                bytes_freed += meta.len();
            }
            let _ = fs::remove_file(entry.path());
        }
    }
    
    // Remove directory itself
    let _ = fs::remove_dir(dir);
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
        match perform_download(&app_clone).await {
            Ok(_) => {
                IS_DOWNLOADING.store(false, Ordering::SeqCst);
                emit_progress(
                    &app_clone,
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
                    &app_clone,
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

async fn perform_download(app: &AppHandle) -> Result<()> {
    let dest_dir = get_offline_dir();
    fs::create_dir_all(&dest_dir)?;

    // We estimate the sizes:
    // sherpa-onnx archive: ~24 MB
    // model: ~239 MB
    // tokens: ~300 KB
    // Total: ~263 MB
    let total_bytes: u64 = 263_500_000;
    let mut bytes_downloaded: u64 = 0;

    // Create client
    let client = reqwest::Client::new();

    // 1. Download sherpa-onnx archive
    let archive_path = dest_dir.join("sherpa-onnx-win-x64.tar.bz2");
    download_file_to_path(
        &client,
        SHERPA_ONNX_URL,
        &archive_path,
        "sherpa-onnx binaries",
        &mut bytes_downloaded,
        total_bytes,
        app,
    )
    .await?;

    // 2. Extract sherpa-onnx archive using Windows built-in tar
    emit_progress(
        app,
        DownloadProgressPayload {
            status: "extracting".to_string(),
            progress: (bytes_downloaded as f64 / total_bytes as f64 * 100.0).min(99.0),
            bytes_downloaded,
            total_bytes,
            current_file: "extracting binaries".to_string(),
            error_message: None,
        },
    );

    let temp_extract_dir = dest_dir.join("temp_extract");
    fs::create_dir_all(&temp_extract_dir)?;

    log::info!("Extracting ASR binary archive to {:?}", temp_extract_dir);
    let output = tokio::process::Command::new("tar")
        .args(&[
            "-xjf",
            archive_path.to_str().unwrap(),
            "-C",
            temp_extract_dir.to_str().unwrap(),
        ])
        .output()
        .await
        .map_err(|e| anyhow!("Failed to run tar command: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("Failed to extract archive: {}", stderr));
    }

    // Copy binary and DLLs recursively
    copy_extracted_files(&temp_extract_dir, &dest_dir)?;

    // Clean up extraction temp directory and tar archive
    let _ = fs::remove_dir_all(&temp_extract_dir);
    let _ = fs::remove_file(&archive_path);

    // 3. Download SenseVoice model.int8.onnx
    let model_path = dest_dir.join("model.int8.onnx");
    download_file_to_path(
        &client,
        SENSEVOICE_MODEL_URL,
        &model_path,
        "SenseVoice model",
        &mut bytes_downloaded,
        total_bytes,
        app,
    )
    .await?;

    // 4. Download SenseVoice tokens.txt
    let tokens_path = dest_dir.join("tokens.txt");
    download_file_to_path(
        &client,
        SENSEVOICE_TOKENS_URL,
        &tokens_path,
        "SenseVoice tokens",
        &mut bytes_downloaded,
        total_bytes,
        app,
    )
    .await?;

    Ok(())
}

async fn download_file_to_path(
    client: &reqwest::Client,
    url: &str,
    dest_path: &Path,
    file_label: &str,
    bytes_downloaded: &mut u64,
    total_bytes: u64,
    app: &AppHandle,
) -> Result<()> {
    if DOWNLOAD_CANCELLED.load(Ordering::SeqCst) {
        return Err(anyhow!("Download cancelled by user"));
    }

    log::info!("Downloading {} from {}", file_label, url);
    let tmp_path = dest_path.with_extension("tmp");
    let mut response = client.get(url).send().await?;

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

