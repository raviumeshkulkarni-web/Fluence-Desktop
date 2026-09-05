// Fluence Windows — Moonshine v2 sidecar server (experiment).
//
// Serves the official Moonshine v2 streaming models (small/medium) over the
// SAME localhost WebSocket protocol as the sherpa-onnx sidecar, so the main
// app needs no protocol changes:
//
//   client -> server: binary [u32LE sample_rate][u32LE byte_len][f32 ...]
//   server -> client: text {"text": "..."} (or raw text)
//   client -> server: text "Done", then close
//
// Whole-utterance mode only (`moonshine_transcribe_without_streaming`):
// the app UI never shows live partials.
//
// CLI: moonshine-v2-server --port=6006 --model-dir=<8 .ort/bin/json files>
//      --arch=small|medium

use futures_util::{SinkExt, StreamExt};
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::sync::Arc;
use tokio::sync::Mutex;

// Must match moonshine-c-api.h (verified against the v0.1.5 bundle header).
const MOONSHINE_HEADER_VERSION: i32 = 30000;
const ARCH_SMALL_STREAMING: u32 = 4;
const ARCH_MEDIUM_STREAMING: u32 = 5;

/// One transcript line. Only `text` (offset 0) is read; the remaining fields
/// replicate the C layout so array indexing strides correctly. A unit test
/// pins the expected 88-byte size.
#[repr(C)]
struct TranscriptLine {
    text: *const c_char,
    audio_data: *const f32,
    audio_data_count: usize,
    start_time: f32,
    duration: f32,
    id: u64,
    is_complete: i8,
    is_updated: i8,
    is_new: i8,
    has_text_changed: i8,
    have_speakers_changed: i8,
    speaker_spans: *const c_void,
    speaker_span_count: u64,
    last_transcription_latency_ms: u32,
    words: *const c_void,
    word_count: u64,
}

#[repr(C)]
struct Transcript {
    lines: *mut TranscriptLine,
    line_count: u64,
}

// Compile-time layout contract against moonshine-c-api.h (vendor v0.1.5,
// header version 30000; see VENDOR.json). The unit test below re-checks
// this at test time, but tests don't run on every build — these asserts
// fail the BUILD itself if anyone edits the struct. If the vendor header
// is ever bumped, re-verify every offset against moonshine-c-api.h first:
// a same-size field reorder would keep size_of at 88 while moving `text`
// off offset 0, and only the offset assert catches that.
const _: () = assert!(std::mem::size_of::<TranscriptLine>() == 88);
const _: () = assert!(std::mem::size_of::<Transcript>() == 16);
const _: () = assert!(std::mem::offset_of!(TranscriptLine, text) == 0);

extern "C" {
    fn moonshine_load_transcriber_from_files(
        path: *const c_char,
        model_arch: u32,
        options: *const c_void,
        options_count: u64,
        moonshine_version: i32,
    ) -> i32;
    fn moonshine_transcribe_without_streaming(
        handle: i32,
        audio_data: *mut f32,
        audio_length: u64,
        sample_rate: i32,
        flags: u32,
        out_transcript: *mut *mut Transcript,
    ) -> i32;
    fn moonshine_free_transcriber(handle: i32);
    fn moonshine_error_to_string(error: i32) -> *const c_char;
}

fn error_string(code: i32) -> String {
    unsafe {
        let ptr = moonshine_error_to_string(code);
        if ptr.is_null() {
            return format!("moonshine error {code}");
        }
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
}

struct Transcriber {
    handle: i32,
    // The native core serializes calls on one transcriber; guard explicitly
    // so concurrent connections queue instead of overlapping.
    lock: Mutex<()>,
}

impl Transcriber {
    fn load(model_dir: &str, arch: u32) -> Result<Self, String> {
        let dir =
            CString::new(model_dir).map_err(|e| format!("invalid model dir '{model_dir}': {e}"))?;
        let handle = unsafe {
            moonshine_load_transcriber_from_files(
                dir.as_ptr(),
                arch,
                std::ptr::null(),
                0,
                MOONSHINE_HEADER_VERSION,
            )
        };
        if handle < 0 {
            return Err(format!(
                "moonshine_load_transcriber_from_files failed: {}",
                error_string(handle)
            ));
        }
        Ok(Self {
            handle,
            lock: Mutex::new(()),
        })
    }

    async fn transcribe(&self, samples: Vec<f32>, sample_rate: u32) -> Result<String, String> {
        let _guard = self.lock.lock().await;
        let handle = self.handle;
        // The blocking call runs on a blocking thread; the native call takes
        // `*mut f32` but does not retain it.
        let (code, texts) = tokio::task::spawn_blocking(move || {
            let mut samples = samples;
            let mut transcript: *mut Transcript = std::ptr::null_mut();
            let code = unsafe {
                moonshine_transcribe_without_streaming(
                    handle,
                    samples.as_mut_ptr(),
                    samples.len() as u64,
                    sample_rate as i32,
                    0,
                    &mut transcript,
                )
            };
            if code != 0 || transcript.is_null() {
                return (code, Vec::new());
            }
            let mut texts = Vec::new();
            unsafe {
                let count = (*transcript).line_count as usize;
                for i in 0..count {
                    let line = (*transcript).lines.add(i);
                    let text_ptr = (*line).text;
                    if !text_ptr.is_null() {
                        texts.push(CStr::from_ptr(text_ptr).to_string_lossy().into_owned());
                    }
                }
            }
            (0, texts)
        })
        .await
        .map_err(|e| format!("transcription task failed: {e}"))?;
        if code != 0 {
            return Err(format!("transcribe failed: {}", error_string(code)));
        }
        Ok(texts
            .iter()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" "))
    }
}

impl Drop for Transcriber {
    fn drop(&mut self) {
        unsafe {
            moonshine_free_transcriber(self.handle);
        }
    }
}

fn parse_arg(prefix: &str) -> Option<String> {
    std::env::args()
        .find(|a| a.starts_with(prefix))
        .map(|a| a[prefix.len()..].to_string())
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    transcriber: Arc<Transcriber>,
) -> Result<(), String> {
    let mut ws = tokio_tungstenite::accept_async(stream)
        .await
        .map_err(|e| format!("websocket accept failed: {e}"))?;

    // One utterance per connection: binary payload, then text result.
    let payload = loop {
        match ws.next().await {
            Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(data))) => break data,
            Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) => return Ok(()),
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(format!("websocket read failed: {e}")),
            None => return Ok(()),
        }
    };
    if payload.len() < 8 {
        return Err("payload too short for [rate][len] header".to_string());
    }
    let sample_rate = u32::from_le_bytes(payload[0..4].try_into().unwrap());
    let byte_len = u32::from_le_bytes(payload[4..8].try_into().unwrap()) as usize;
    if sample_rate != 16_000 {
        return Err(format!(
            "unsupported sample rate {sample_rate}, expected 16000"
        ));
    }
    if payload.len() < 8 + byte_len || byte_len % 4 != 0 {
        return Err("payload length mismatch".to_string());
    }
    let float_count = byte_len / 4;
    let mut samples = Vec::with_capacity(float_count);
    for chunk in payload[8..8 + byte_len].chunks_exact(4) {
        samples.push(f32::from_le_bytes(chunk.try_into().unwrap()));
    }

    match transcriber.transcribe(samples, sample_rate).await {
        Ok(text) => {
            let reply = serde_json::json!({ "text": text }).to_string();
            let _ = ws
                .send(tokio_tungstenite::tungstenite::Message::Text(reply.into()))
                .await;
            // Wait for the client's "Done" (mirrors the sherpa flow), then close.
            let _ = tokio::time::timeout(std::time::Duration::from_secs(30), async {
                while let Some(msg) = ws.next().await {
                    if matches!(
                        msg,
                        Ok(tokio_tungstenite::tungstenite::Message::Text(_))
                            | Ok(tokio_tungstenite::tungstenite::Message::Close(_))
                    ) {
                        break;
                    }
                }
            })
            .await;
            let _ = ws.close(None).await;
            Ok(())
        }
        Err(e) => {
            eprintln!("transcription failed: {e}");
            // Close without a text frame: the client treats an empty
            // response as an error, same as with the sherpa server.
            let _ = ws.close(None).await;
            Err(e)
        }
    }
}

#[tokio::main]
async fn main() {
    let port: u16 = parse_arg("--port=")
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| {
            eprintln!("missing or invalid --port=N");
            std::process::exit(2);
        });
    let model_dir = parse_arg("--model-dir=").unwrap_or_else(|| {
        eprintln!("missing --model-dir=PATH");
        std::process::exit(2);
    });
    let arch = match parse_arg("--arch=").as_deref() {
        Some("small") => ARCH_SMALL_STREAMING,
        Some("medium") => ARCH_MEDIUM_STREAMING,
        other => {
            eprintln!("missing or invalid --arch=small|medium (got {other:?})");
            std::process::exit(2);
        }
    };

    eprintln!("moonshine-v2-server: loading arch {arch} from {model_dir}");
    let transcriber = match Transcriber::load(&model_dir, arch) {
        Ok(t) => Arc::new(t),
        Err(e) => {
            eprintln!("moonshine-v2-server: model load failed: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("moonshine-v2-server: model loaded, listening on 127.0.0.1:{port}");

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap_or_else(|e| {
            eprintln!("moonshine-v2-server: bind failed: {e}");
            std::process::exit(1);
        });
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let transcriber = transcriber.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, transcriber).await {
                        eprintln!("moonshine-v2-server: connection error: {e}");
                    }
                });
            }
            Err(e) => eprintln!("moonshine-v2-server: accept failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_line_layout_matches_c_header() {
        // Hand-computed from moonshine-c-api.h v0.1.5 (MSVC x64 LP64):
        // 8+8+8+4+4+8 = 40, five i8 = 45 -> pad to 48, ptr = 56, u64 = 64,
        // u32 = 68 -> pad to 72, ptr = 80, u64 = 88. If the header ever
        // changes, this fails loudly instead of mis-striding lines.
        assert_eq!(std::mem::size_of::<TranscriptLine>(), 88);
        assert_eq!(std::mem::size_of::<Transcript>(), 16);
    }

    #[test]
    fn arch_ids_match_official_header_and_android() {
        assert_eq!(ARCH_SMALL_STREAMING, 4);
        assert_eq!(ARCH_MEDIUM_STREAMING, 5);
        assert_eq!(MOONSHINE_HEADER_VERSION, 30000);
    }
}
