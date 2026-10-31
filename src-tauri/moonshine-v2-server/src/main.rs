// Fluence Windows — Moonshine v2 sidecar server (experiment).
//
// Serves the official Moonshine v2 streaming models (small/medium) over the
// same localhost WebSocket protocol family as the sherpa-onnx sidecar:
//
//   readiness probe:
//     client -> server: text "probe:<auth-token>"
//     server -> client: text {"ok":true,"server":"moonshine-v2"}, then close
//     (the parent only treats this reply as proof that the spawned sidecar
//     owns the port and knows the spawn secret; a bare TCP connect is not
//     identity)
//   audio:
//     client -> server: binary [32-byte auth token][u32LE sample_rate]
//                       [u32LE byte_len][f32 ...]  (token == handshake value)
//     server -> client: text {"text": "..."} (or raw text)
//     client -> server: text "Done", then close
//
// Whole-utterance mode only (`moonshine_transcribe_without_streaming`):
// the app UI never shows live partials.
//
// Token delivery: the PARENT pipes a freshly generated uuid token to this
// process's stdin, then closes it; the token is consumed once and never
// logged. `--auth-token=<hex>` exists only as a manual/debug fallback
// because a command-line value is readable by same-user processes via
// Win32_Process — keep it out of any automated path.
//
// Concurrency: at most MAX_CONCURRENT client connections; additional
// connections receive an HTTP 503 refusal the parent can surface as a
// distinct "busy" error. Beyond the shared secret, process identity is
// NOT otherwise verified (this is the documented residual risk, same
// threat class as the still-unhardened Sherpa sidecar).

use futures_util::{SinkExt, StreamExt};
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, Semaphore};

// Must match moonshine-c-api.h (verified against the v0.1.5 bundle header).
const MOONSHINE_HEADER_VERSION: i32 = 30000;
const ARCH_SMALL_STREAMING: u32 = 4;
const ARCH_MEDIUM_STREAMING: u32 = 5;

/// Required first bytes of every client audio payload and every probe
/// text. CONTRACT: the parent generates `uuid::Uuid::new_v4().simple()`,
/// which is exactly this many lowercase hex characters. If either side
/// changes, wire tests on both sides must change together.
const TOKEN_LEN: usize = 32;

/// Max concurrent client connections. Each connection is one utterance,
/// and utterances serialize on the model's internal mutex, so 8 covers
/// every legitimate flow while bounding memory. Exhaustion is observable
/// (HTTP 503), never a silent drop.
const MAX_CONCURRENT: usize = 8;

/// Readiness-probe reply. Must name this server: the parent rejects any
/// other body.
const PROBE_REPLY: &str = r#"{"ok":true,"server":"moonshine-v2"}"#;

/// Validate the mandatory auth-token prefix of a client binary payload.
/// Returns the payload WITHOUT the token (the [rate][len][samples] body)
/// or a stable error string the parent surfaces.
fn validate_token_prefix<'a>(full: &'a [u8], token: &'a str) -> Result<&'a [u8], &'static str> {
    if full.len() < TOKEN_LEN + 8 {
        return Err("payload too short for auth token + [rate][len] header");
    }
    if full[0..TOKEN_LEN] != token.as_bytes()[0..TOKEN_LEN] {
        return Err("unauthorized: missing or invalid auth token");
    }
    Ok(&full[TOKEN_LEN..])
}

/// Server response written to a raw TCP connection when the connection
/// limiter is exhausted. A real HTTP 503 with `Connection: close` lets the
/// client surface a distinct "busy" error instead of a bare socket drop.
fn capacity_refusal() -> String {
    const BODY: &str = "at capacity";
    format!(
        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        BODY.len(),
        BODY
    )
}

/// Reserve one of MAX_CONCURRENT connection slots BEFORE the task is
/// spawned, so the limit holds exactly even under concurrent accepts.
/// The permit's Drop returns the slot on every exit path, including panics.
fn reserve_permit(permits: &Arc<Semaphore>) -> Option<tokio::sync::OwnedSemaphorePermit> {
    permits.clone().try_acquire_owned().ok()
}

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
    token: String,
) -> Result<(), String> {
    // tungstenite's defaults (64 MiB message / 16 MiB frame) comfortably fit
    // the largest legitimate utterance (10 min @ 16 kHz f32 = ~38.4 MB,
    // fragmented to <=16 MiB frames by the client library). A behavior test
    // in this module asserts that arithmetic so a future limit change
    // cannot silently clip recordings.
    let mut ws = tokio_tungstenite::accept_async(stream)
        .await
        .map_err(|e| format!("websocket accept failed: {e}"))?;

    // Readiness probe first: text "probe:<token>" answers with a named
    // reply; anything else that is text is refused (audio payloads are
    // binary, and only the sidecar knows the token). Audio then proceeds
    // as one binary message carrying the same token ahead of the header.
    let full = loop {
        match ws.next().await {
            Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                if let Some(probe) = text.strip_prefix("probe:") {
                    if probe == token {
                        let _ = ws
                            .send(tokio_tungstenite::tungstenite::Message::Text(
                                PROBE_REPLY.to_string(),
                            ))
                            .await;
                        let _ = ws.close(None).await;
                        return Ok(());
                    }
                    return Err("unauthorized: probe token mismatch".to_string());
                }
                return Err(format!(
                    "unexpected text message: send 'probe:<token>' or a binary audio payload (got {} bytes of text)",
                    text.len()
                ));
            }
            Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(data))) => break data,
            Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) => return Ok(()),
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(format!("websocket read failed: {e}")),
            None => return Ok(()),
        }
    };
    let payload = match validate_token_prefix(&full, &token) {
        Ok(rest) => rest,
        Err(e) => return Err(e.to_string()),
    };
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
    // Token acquisition. Prefer the private stdin handshake (the parent
    // writes the fresh token, then closes stdin; nothing of the token is
    // logged). `--auth-token=<hex>` remains as a manual/debug fallback —
    // explicitly NOT for automated spawning, because command lines are
    // readable by same-user processes via Win32_Process.
    let token = if let Some(arg_token) = parse_arg("--auth-token=") {
        eprintln!("moonshine-v2-server: token from CLI arg (manual mode)");
        arg_token
    } else {
        let read_token = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let mut line = String::new();
            let read = tokio::io::BufReader::new(tokio::io::stdin())
                .read_line(&mut line)
                .await;
            read.map(|n| (n, line))
        })
        .await;
        match read_token {
            Ok(Ok((n, line))) if n > 0 => line.trim_end_matches(['\n', '\r']).to_string(),
            Ok(Ok(_)) | Ok(Err(_)) | Err(_) => {
                eprintln!("missing --auth-token arg and empty/timed-out stdin handshake");
                std::process::exit(2);
            }
        }
    };
    // No value is logged: the token exists to keep other local processes
    // away from the transcript channel.
    if token.len() < TOKEN_LEN {
        eprintln!("invalid auth token (expected at least {TOKEN_LEN} chars)");
        std::process::exit(2);
    }

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
    let conn_permits = Arc::new(Semaphore::new(MAX_CONCURRENT));
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                // Reserve BEFORE spawning: the limit now holds exactly even
                // under concurrent accepts (a load-then-increment race used
                // to let the bound slip).
                let permit = match reserve_permit(&conn_permits) {
                    Some(permit) => permit,
                    None => {
                        eprintln!("moonshine-v2-server: connection refused: at capacity");
                        // Observable refusal rather than a silent drop: the
                        // HTTP 503 response lets the client surface a
                        // distinct "busy" error.
                        let mut refuse = stream;
                        let _ = refuse.write_all(capacity_refusal().as_bytes()).await;
                        let _ = refuse.shutdown().await;
                        continue;
                    }
                };
                let transcriber = transcriber.clone();
                let token = token.clone();
                tokio::spawn(async move {
                    // Released on every exit path (success, error, panic) by
                    // moving the permit's Drop into the task.
                    let _permit = permit;
                    if let Err(e) = handle_connection(stream, transcriber, token).await {
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

    // ── Auth-token prefix contract (extracted helper, behavior-tested) ──
    // The parent generates uuid::Uuid::new_v4().simple() — exactly 32 hex
    // chars — and prefixes it to every audio payload; these tests pin the
    // sidecar half of that shared contract.

    #[test]
    fn token_prefix_rejects_empty_and_short_payloads() {
        let token = "a".repeat(TOKEN_LEN);
        assert!(validate_token_prefix(&[], &token).is_err());
        let one_short = vec![0u8; TOKEN_LEN + 7];
        let result = validate_token_prefix(&one_short, &token).unwrap_err();
        assert_eq!(
            result,
            "payload too short for auth token + [rate][len] header"
        );
    }

    #[test]
    fn token_prefix_rejects_wrong_token() {
        let token = "a".repeat(TOKEN_LEN);
        let wrong = vec![b'x'; TOKEN_LEN + 8];
        let result = validate_token_prefix(&wrong, &token).unwrap_err();
        assert_eq!(result, "unauthorized: missing or invalid auth token");
    }

    #[test]
    fn token_prefix_accepts_exact_minimum_length() {
        // 32-byte token + 8-byte [rate][len] header is the smallest accepted
        // payload (an empty utterance) — the helper must accept it and not
        // demand sample bytes.
        let mut full = "a".repeat(TOKEN_LEN).into_bytes();
        full.extend_from_slice(&[0u8; 8]);
        let token = "a".repeat(TOKEN_LEN);
        let rest = validate_token_prefix(&full, &token).unwrap();
        assert_eq!(rest.len(), 8);
    }

    #[test]
    fn token_prefix_strips_token_and_preserves_header_and_samples() {
        let token = "a".repeat(TOKEN_LEN);
        let mut full = token.clone().into_bytes();
        full.extend_from_slice(&16_000u32.to_le_bytes());
        full.extend_from_slice(&4u32.to_le_bytes());
        full.extend_from_slice(&0.5f32.to_le_bytes());
        let rest = validate_token_prefix(&full, &token).unwrap();
        assert_eq!(rest.len(), 12);
        assert_eq!(&rest[0..4], &16_000u32.to_le_bytes());
        assert_eq!(&rest[4..8], &4u32.to_le_bytes());
        assert_eq!(&rest[8..12], &0.5f32.to_le_bytes());
    }

    #[test]
    fn token_contract_is_pinned_at_32_hex_chars() {
        assert_eq!(TOKEN_LEN, 32);
    }

    // ── Capacity refusal shape ────────────────────────────────────────

    #[test]
    fn capacity_refusal_is_a_valid_http_503_close() {
        let refusal = capacity_refusal();
        assert!(refusal.starts_with("HTTP/1.1 503 "), "must be a 503");
        assert!(refusal.contains("Connection: close"), "must close cleanly");
        let (head, body) = refusal.split_once("\r\n\r\n").unwrap();
        let cl = body.len().to_string();
        assert!(
            head.to_ascii_lowercase()
                .contains(&format!("content-length: {cl}")),
            "Content-Length must match the real body size"
        );
    }

    // ── Message-size contract ─────────────────────────────────────────

    #[test]
    fn max_utterance_fits_backwards_compatible_socket_bounds() {
        // Mirrors transcribe::MAX_OFFLINE_SAMPLES (600 s @ 16 kHz f32 = 38.4
        // MB). The sidecar now relies on tungstenite's DEFAULT limits
        // (64 MiB message / 16 MiB frame, client auto-fragments) — this test
        // pins that the shipped default still fits the app maximum, so a
        // tungstenite upgrade cannot silently clip recordings. If the app
        // limit ever grows, extend or configure the bounds deliberately.
        const MAX_LIB_SAMPLES: usize = 9_600_000;
        const DEFAULT_MAX_MESSAGE: usize = 64 * 1024 * 1024;
        const WIRE_OVERHEAD: usize = TOKEN_LEN + 8;
        assert!(WIRE_OVERHEAD + MAX_LIB_SAMPLES * 4 <= DEFAULT_MAX_MESSAGE);
    }

    // ── Connection limiter ────────────────────────────────────────────

    #[tokio::test]
    async fn capacity_limiter_is_exact_and_released() {
        let permits = Arc::new(Semaphore::new(MAX_CONCURRENT));
        let mut held = Vec::new();
        for _ in 0..MAX_CONCURRENT {
            held.push(reserve_permit(&permits).expect("up to MAX slots must be reservable"));
        }
        assert!(
            reserve_permit(&permits).is_none(),
            "the (MAX+1)th reservation must fail"
        );
        drop(held.pop().unwrap());
        assert!(
            reserve_permit(&permits).is_some(),
            "a released slot must be reservable again (Drop release)"
        );
    }
}
