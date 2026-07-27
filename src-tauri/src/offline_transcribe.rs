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
use std::time::Instant;

const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfflineEngine {
    SenseVoice,
    #[serde(rename = "moonshine_base")]
    MoonshineBase,
}

impl OfflineEngine {
    pub fn display_name(&self) -> &str {
        match self {
            Self::SenseVoice => "SenseVoice",
            Self::MoonshineBase => "Moonshine Base",
        }
    }

    pub fn dir_name(&self) -> &str {
        match self {
            Self::SenseVoice => "sensevoice_v2",
            Self::MoonshineBase => "moonshine_base",
        }
    }
}

impl std::fmt::Display for OfflineEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SenseVoice => write!(f, "sensevoice"),
            Self::MoonshineBase => write!(f, "moonshine_base"),
        }
    }
}

impl std::str::FromStr for OfflineEngine {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "sensevoice" => Ok(Self::SenseVoice),
            "moonshine_base" => Ok(Self::MoonshineBase),
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

pub async fn ensure_server_running(engine: OfflineEngine) -> Result<u16> {
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

    let exe_path = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("Fluence")
        .join("bin")
        .join("sensevoice_v2")
        .join("sherpa-onnx-offline-websocket-server.exe");

    if !exe_path.exists() {
        return Err(anyhow!(
            "Offline transcription engine is not installed. Please download it in Settings."
        ));
    }

    // Verify binary integrity before execution
    let exe_hash = crate::offline_downloader::manifest_binary_hash(
        "sherpa-onnx-offline-websocket-server.exe",
    )?;
    crate::offline_downloader::verify_sha256(&exe_path, exe_hash).map_err(|e| {
        anyhow!(
            "Binary integrity check failed: {}. Please re-download the model in Settings.",
            e
        )
    })?;

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
        OfflineEngine::MoonshineBase => {
            let required = [
                "preprocess.onnx",
                "encode.int8.onnx",
                "uncached_decode.int8.onnx",
                "cached_decode.int8.onnx",
                "tokens.txt",
            ];
            for file in &required {
                let p = offline_dir.join(file);
                if !p.exists() {
                    return Err(anyhow!(
                        "Moonshine Base model file '{}' is missing. Please download the model in Settings.",
                        file
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
    log::info!(
        "Starting sherpa-onnx websocket server (engine: {}) on port {}",
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
            let tokens_arg = format!("--tokens={}", tokens_path.to_str().unwrap());
            let model_arg = format!("--sense-voice-model={}", model_path.to_str().unwrap());
            cmd.args([&tokens_arg, &model_arg, &port_arg, &threads_arg]);
        }
        OfflineEngine::MoonshineBase => {
            let tokens_path = offline_dir.join("tokens.txt");
            let tokens_arg = format!("--tokens={}", tokens_path.to_str().unwrap());
            let preprocessor_arg = format!(
                "--moonshine-preprocessor={}",
                offline_dir.join("preprocess.onnx").to_str().unwrap()
            );
            let encoder_arg = format!(
                "--moonshine-encoder={}",
                offline_dir.join("encode.int8.onnx").to_str().unwrap()
            );
            let uncached_decoder_arg = format!(
                "--moonshine-uncached-decoder={}",
                offline_dir
                    .join("uncached_decode.int8.onnx")
                    .to_str()
                    .unwrap()
            );
            let cached_decoder_arg = format!(
                "--moonshine-cached-decoder={}",
                offline_dir
                    .join("cached_decode.int8.onnx")
                    .to_str()
                    .unwrap()
            );
            cmd.args([
                &tokens_arg,
                &preprocessor_arg,
                &encoder_arg,
                &uncached_decoder_arg,
                &cached_decoder_arg,
                &port_arg,
                &threads_arg,
            ]);
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
            "sherpa-onnx server failed to start on port {}",
            port
        ));
    }

    log::info!(
        "sherpa-onnx server is ready on port {} (engine: {})",
        port,
        engine.display_name()
    );
    Ok(port)
}

pub async fn transcribe_samples(samples: Vec<f32>, engine: OfflineEngine) -> Result<String> {
    let port = ensure_server_running(engine).await?;
    *LAST_USED.lock().unwrap() = Instant::now();

    let url = format!("ws://127.0.0.1:{}", port);
    let (mut ws_stream, _) = tokio_tungstenite::connect_async(&url)
        .await
        .map_err(|e| anyhow!("Failed to connect to local ASR server: {}", e))?;

    // Build the custom sherpa-onnx binary payload
    // Format: [int32LE sample_rate][int32LE num_audio_bytes][float32 samples...]
    let sample_rate = 16000u32;
    let byte_len = (samples.len() * 4) as u32;
    let mut payload = Vec::with_capacity(8 + samples.len() * 4);
    payload.extend_from_slice(&sample_rate.to_le_bytes());
    payload.extend_from_slice(&byte_len.to_le_bytes());
    for &sample in &samples {
        payload.extend_from_slice(&sample.to_le_bytes());
    }

    // Send audio payload
    ws_stream
        .send(tokio_tungstenite::tungstenite::Message::Binary(payload))
        .await
        .map_err(|e| anyhow!("Failed to send audio to ASR server: {}", e))?;

    // Per sherpa-onnx protocol: server decodes after receiving all bytes,
    // sends text result, THEN client sends "Done" to close.
    // Do NOT send "Done" before receiving the result — it closes the connection.

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
