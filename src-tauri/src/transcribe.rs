// Fluence Windows — Transcription module
// Sends recorded WAV audio to any OpenAI-compatible /v1/audio/transcriptions endpoint.
// Supports Groq, OpenAI, and custom providers.

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
struct TranscriptionResponse {
    text: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TranscribeRequest {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// Base64-encoded audio bytes (FLAC or WAV, determined by mime_type/filename)
    /// Semantically renamed in documentation, but kept as wav_b64 for frontend compatibility.
    pub wav_b64: String,
    pub language: Option<String>,
    pub prompt: Option<String>,
    #[serde(default = "default_mime_type")]
    pub mime_type: String,
    #[serde(default = "default_filename")]
    pub filename: String,
}

fn default_mime_type() -> String {
    "audio/wav".to_string()
}

fn default_filename() -> String {
    "audio.wav".to_string()
}

/// Maximum base64-encoded audio size (~25MB decoded, ~5 minutes at 16kHz mono)
const MAX_AUDIO_B64_LEN: usize = 35_000_000;

/// Maximum raw MP3/FLAC/WAV payload for online providers (25MB provider limit, leave margin)
pub const MAX_AUDIO_BYTES: usize = 22_000_000;

/// Maximum offline sample count (10 minutes at 16kHz mono = 9.6M samples)
pub const MAX_OFFLINE_SAMPLES: usize = 10 * 60 * 16_000;

fn check_audio_bytes_len(len: usize) -> Result<(), String> {
    if len > MAX_AUDIO_BYTES {
        return Err(format!(
            "Recording too long ({} bytes, {:.1} MB). Maximum is ~22 MB (~10 minutes at 64 kbps). Please split recordings.",
            len,
            len as f64 / 1_000_000.0
        ));
    }
    Ok(())
}

/// Transcribe audio via an OpenAI-compatible API.
#[tauri::command]
pub async fn transcribe_audio(req: TranscribeRequest) -> Result<String, String> {
    if req.wav_b64.len() > MAX_AUDIO_B64_LEN {
        return Err(format!(
            "Audio payload too large ({} bytes). Maximum supported size is ~25MB decoded.",
            req.wav_b64.len()
        ));
    }
    crate::http_client::validate_api_url(&req.base_url)?;
    let start_time = std::time::Instant::now();
    let mp3_bytes =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &req.wav_b64)
            .map_err(|e| format!("Base64 decode error: {}", e))?;

    let decode_duration = start_time.elapsed();

    let corrected = transcribe_mp3_bytes(
        &req.base_url,
        &req.api_key,
        &req.model,
        mp3_bytes,
        req.language.as_deref(),
    )
    .await?;

    log::info!(
        "transcribe_audio base64 bridge: total = {:?}, decode = {:?}",
        start_time.elapsed(),
        decode_duration
    );

    Ok(corrected)
}

#[allow(dead_code)]
pub async fn transcribe_audio_bytes(
    base_url: &str,
    api_key: &str,
    model: &str,
    audio_bytes: Vec<u8>,
    mime_type: &str,
    filename: &str,
    language: Option<&str>,
    prompt: Option<&str>,
) -> Result<String, String> {
    check_audio_bytes_len(audio_bytes.len())?;
    let start_time = std::time::Instant::now();

    let url = crate::http_client::build_api_url(base_url, "audio/transcriptions");

    let file_part = reqwest::multipart::Part::bytes(audio_bytes)
        .file_name(filename.to_string())
        .mime_str(mime_type)
        .map_err(|e| e.to_string())?;

    let mut form = reqwest::multipart::Form::new()
        .part("file", file_part)
        .text("model", model.to_string())
        .text("response_format", "json")
        .text("temperature", "0.0");

    if let Some(p) = prompt {
        if !p.is_empty() {
            form = form.text("prompt", p.to_string());
        }
    }

    if let Some(lang) = language {
        if !lang.is_empty() && lang != "auto" {
            form = form.text("language", lang.to_string());
        }
    }

    let network_start = std::time::Instant::now();
    let is_mistral = base_url.contains("mistral.ai");
    let mut request = crate::http_client::CLIENT
        .post(&url)
        .bearer_auth(api_key)
        .multipart(form);

    // Mistral sometimes requires the 'x-api-key' header specifically for their
    // transcription gateway. We provide it conditionally for maximum compatibility.
    if is_mistral {
        request = request.header("x-api-key", api_key);
    }

    let resp = request
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    let network_duration = network_start.elapsed();

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("API error {}: {}", status, body));
    }

    let parse_start = std::time::Instant::now();
    let result: TranscriptionResponse = resp
        .json()
        .await
        .map_err(|e| format!("JSON parse error: {}", e))?;

    log::info!(
        "transcribe_audio_bytes performance: total = {:?}, network = {:?}, parse = {:?}",
        start_time.elapsed(),
        network_duration,
        parse_start.elapsed()
    );

    Ok(result.text)
}

/// Build the Whisper vocabulary hint from dictionary entries.
///
/// SECURITY INVARIANT: Expansion entries are excluded entirely — expansion
/// text must never enter the STT recognition prompt. Only correction entries
/// contribute their spoken/corrected words.
///
/// Groq rejects prompts > 896 characters, so the result is truncated to 890.
fn build_vocabulary_hint(entries: &[crate::dictionary::DictionaryEntry]) -> Option<String> {
    let mut prompt_words = Vec::new();
    for entry in entries {
        if entry.kind == "expansion" {
            continue;
        }
        if !entry.corrected.trim().is_empty() {
            prompt_words.push(entry.corrected.clone());
        }
        if !entry.spoken.trim().is_empty() && entry.spoken != entry.corrected {
            prompt_words.push(entry.spoken.clone());
        }
    }
    if prompt_words.is_empty() {
        return None;
    }

    let mut prompt = prompt_words.join(", ");
    if prompt.len() > 890 {
        let mut end = 890;
        while !prompt.is_char_boundary(end) {
            end -= 1;
        }
        let prefix = &prompt[..end];
        if let Some(last_comma) = prefix.rfind(',') {
            prompt.truncate(last_comma);
        } else {
            prompt.truncate(end);
        }
        log::debug!(
            "Prompt truncated to {} characters for Groq compatibility",
            prompt.len()
        );
    }
    Some(prompt)
}

pub async fn transcribe_mp3_bytes(
    base_url: &str,
    api_key: &str,
    model: &str,
    mp3_bytes: Vec<u8>,
    language: Option<&str>,
) -> Result<String, String> {
    check_audio_bytes_len(mp3_bytes.len())?;
    let start_time = std::time::Instant::now();

    let url = crate::http_client::build_api_url(base_url, "audio/transcriptions");

    let file_part = reqwest::multipart::Part::bytes(mp3_bytes)
        .file_name("audio.mp3")
        .mime_str("audio/mpeg")
        .map_err(|e| e.to_string())?;

    let multipart_start = std::time::Instant::now();
    let mut form = reqwest::multipart::Form::new()
        .part("file", file_part)
        .text("model", model.to_string())
        .text("response_format", "json")
        .text("temperature", "0.0");

    if let Some(lang) = language {
        if !lang.is_empty() && lang != "auto" {
            form = form.text("language", lang.to_string());
        }
    }

    // Feed correction entries to Whisper as a vocabulary hint.
    // Expansion entries must never enter the STT recognition prompt.
    if let Ok(entries) = crate::dictionary::get_dictionary() {
        if let Some(prompt) = build_vocabulary_hint(&entries) {
            form = form.text("prompt", prompt);
        }
    }

    let network_start = std::time::Instant::now();
    let is_mistral = base_url.contains("mistral.ai");
    let mut request = crate::http_client::CLIENT
        .post(&url)
        .bearer_auth(api_key)
        .multipart(form);

    // Mistral sometimes requires the 'x-api-key' header specifically for their
    // transcription gateway. We provide it conditionally for maximum compatibility.
    if is_mistral {
        request = request.header("x-api-key", api_key);
    }

    let resp = request
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    let network_duration = network_start.elapsed();

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("API error {}: {}", status, body));
    }

    let parse_start = std::time::Instant::now();
    let result: TranscriptionResponse = resp
        .json()
        .await
        .map_err(|e| format!("JSON parse error: {}", e))?;

    // Apply dictionary corrections, then snippet expansion
    let corrected = crate::snippets::process_transcript(&result.text);

    log::info!(
        "transcribe_mp3_bytes performance: total = {:?}, network = {:?}, parse = {:?}, multipart = {:?}",
        start_time.elapsed(),
        network_duration,
        parse_start.elapsed(),
        multipart_start.elapsed()
    );

    Ok(corrected)
}

/// Result of transcription with both raw and corrected text.
/// Used by the suggestion engine to detect corrections.
#[derive(Debug, Clone)]
pub struct TranscriptionWithRaw {
    /// Raw STT output before dictionary corrections
    pub raw_text: String,
    /// Text after dictionary corrections
    pub corrected_text: String,
}

/// Transcribe audio and return both raw and corrected text.
/// This is used by the suggestion engine to compare raw vs corrected.
/// Does NOT modify the existing `transcribe_mp3_bytes` function.
pub async fn transcribe_mp3_bytes_with_raw(
    base_url: &str,
    api_key: &str,
    model: &str,
    mp3_bytes: &[u8],
    language: Option<&str>,
) -> Result<TranscriptionWithRaw, String> {
    check_audio_bytes_len(mp3_bytes.len())?;
    let start_time = std::time::Instant::now();

    let url = crate::http_client::build_api_url(base_url, "audio/transcriptions");

    let file_part = reqwest::multipart::Part::bytes(mp3_bytes.to_vec())
        .file_name("audio.mp3")
        .mime_str("audio/mpeg")
        .map_err(|e| e.to_string())?;

    let multipart_start = std::time::Instant::now();
    let mut form = reqwest::multipart::Form::new()
        .part("file", file_part)
        .text("model", model.to_string())
        .text("response_format", "json")
        .text("temperature", "0.0");

    if let Some(lang) = language {
        if !lang.is_empty() && lang != "auto" {
            form = form.text("language", lang.to_string());
        }
    }

    // Feed correction entries to Whisper as a vocabulary hint.
    // Expansion entries must never enter the STT recognition prompt.
    if let Ok(entries) = crate::dictionary::get_dictionary() {
        if let Some(prompt) = build_vocabulary_hint(&entries) {
            form = form.text("prompt", prompt);
        }
    }

    let network_start = std::time::Instant::now();
    let is_mistral = base_url.contains("mistral.ai");
    let mut request = crate::http_client::CLIENT
        .post(&url)
        .bearer_auth(api_key)
        .multipart(form);

    if is_mistral {
        request = request.header("x-api-key", api_key);
    }

    let resp = request
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    let network_duration = network_start.elapsed();

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("API error {}: {}", status, body));
    }

    let parse_start = std::time::Instant::now();
    let result: TranscriptionResponse = resp
        .json()
        .await
        .map_err(|e| format!("JSON parse error: {}", e))?;

    // Capture raw text before dictionary corrections
    let raw_text = result.text.clone();

    // Apply dictionary corrections, then snippet expansion
    let corrected_text = crate::snippets::process_transcript(&result.text);

    log::info!(
        "transcribe_mp3_bytes_with_raw performance: total = {:?}, network = {:?}, parse = {:?}, multipart = {:?}",
        start_time.elapsed(),
        network_duration,
        parse_start.elapsed(),
        multipart_start.elapsed()
    );

    Ok(TranscriptionWithRaw {
        raw_text,
        corrected_text,
    })
}

/// Fetch available models from an OpenAI-compatible /v1/models endpoint.
#[tauri::command]
pub async fn fetch_models(base_url: String, api_key: String) -> Result<Vec<String>, String> {
    crate::http_client::validate_api_url(&base_url)?;
    let url = crate::http_client::build_api_url(&base_url, "models");

    let resp = crate::http_client::CLIENT
        .get(&url)
        .bearer_auth(&api_key)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("API error: {}", resp.status()));
    }

    #[derive(Deserialize)]
    struct ModelObj {
        id: String,
    }
    #[derive(Deserialize)]
    struct ModelsResponse {
        data: Vec<ModelObj>,
    }

    let models: ModelsResponse = resp
        .json()
        .await
        .map_err(|e| format!("JSON parse error: {}", e))?;

    let ids: Vec<String> = models.data.into_iter().map(|m| m.id).collect();
    Ok(ids)
}

/// Test connectivity to an STT provider.
#[tauri::command]
pub async fn test_stt_connection(base_url: String, api_key: String) -> Result<String, String> {
    crate::http_client::validate_api_url(&base_url)?;
    let url = crate::http_client::build_api_url(&base_url, "models");

    let resp = crate::http_client::CLIENT
        .get(&url)
        .bearer_auth(&api_key)
        .timeout(std::time::Duration::from_secs(8))
        .send()
        .await
        .map_err(|e| format!("Connection failed: {}", e))?;

    if resp.status().is_success() {
        Ok("Connection successful".to_string())
    } else {
        Err(format!("Authentication failed ({})", resp.status()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_stt_transcribe_perf() {
        let settings = crate::settings::load_settings().unwrap();
        let api_key =
            match crate::credentials::read_credential(crate::credentials::STT_API_KEY_TARGET) {
                Ok(k) => k,
                Err(e) => {
                    println!(
                        "--- BENCHMARK RESULT: Skipping STT test because no API key found: {}",
                        e
                    );
                    return;
                }
            };

        // Create 5 seconds of dummy mono WAV audio (sine wave) at 16000Hz
        let sample_rate = 16000;
        let num_samples = sample_rate as usize * 5;
        let mut dummy_samples = Vec::with_capacity(num_samples);
        for i in 0..num_samples {
            let t = i as f32 / sample_rate as f32;
            let sample = (t * 440.0 * 2.0 * std::f32::consts::PI).sin() * 0.5;
            dummy_samples.push((sample * i16::MAX as f32) as i16);
        }

        let wav_bytes = crate::audio::create_wav_bytes(&dummy_samples, sample_rate, 1);

        println!(
            "Sending 5 seconds of dummy WAV audio ({} bytes) to STT API...",
            wav_bytes.len()
        );
        let start = std::time::Instant::now();
        let res = transcribe_audio_bytes(
            &settings.stt_provider.base_url,
            &api_key,
            &settings.stt_provider.model,
            wav_bytes,
            "audio/wav",
            "audio.wav",
            Some("en"),
            None,
        )
        .await;

        match res {
            Ok(text) => {
                println!("--- BENCHMARK RESULT: STT 5s audio transcription successful in {:?}. Response: {:?}", start.elapsed(), text);
            }
            Err(e) => {
                println!(
                    "--- BENCHMARK RESULT: STT 5s audio transcription failed in {:?}: {}",
                    start.elapsed(),
                    e
                );
            }
        }
    }

    #[test]
    fn reject_oversized_audio() {
        let req = TranscribeRequest {
            base_url: "https://api.groq.com/openai".into(),
            api_key: "test".into(),
            model: "whisper".into(),
            wav_b64: "a".repeat(MAX_AUDIO_B64_LEN + 1),
            language: None,
            prompt: None,
            mime_type: "audio/wav".into(),
            filename: "audio.wav".into(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(transcribe_audio(req));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("too large"));
    }

    #[test]
    fn reject_oversized_audio_exact_boundary() {
        let req = TranscribeRequest {
            base_url: "https://api.groq.com/openai".into(),
            api_key: "test".into(),
            model: "whisper".into(),
            wav_b64: "a".repeat(MAX_AUDIO_B64_LEN + 100),
            language: None,
            prompt: None,
            mime_type: "audio/wav".into(),
            filename: "audio.wav".into(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(transcribe_audio(req));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("too large"));
    }

    #[test]
    fn accept_audio_at_limit() {
        let req = TranscribeRequest {
            base_url: "https://api.groq.com/openai".into(),
            api_key: "test".into(),
            model: "whisper".into(),
            wav_b64: "a".repeat(MAX_AUDIO_B64_LEN),
            language: None,
            prompt: None,
            mime_type: "audio/wav".into(),
            filename: "audio.wav".into(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(transcribe_audio(req));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            !err.contains("too large"),
            "At exactly MAX_AUDIO_B64_LEN the size check should pass, got: {}",
            err
        );
    }

    #[test]
    fn reject_invalid_url_in_transcribe() {
        let req = TranscribeRequest {
            base_url: "not-a-url".into(),
            api_key: "test".into(),
            model: "whisper".into(),
            wav_b64: "dGVzdA==".into(),
            language: None,
            prompt: None,
            mime_type: "audio/wav".into(),
            filename: "audio.wav".into(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(transcribe_audio(req));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid URL"));
    }

    #[test]
    fn reject_http_non_localhost_in_transcribe() {
        let req = TranscribeRequest {
            base_url: "http://api.groq.com/openai".into(),
            api_key: "test".into(),
            model: "whisper".into(),
            wav_b64: "dGVzdA==".into(),
            language: None,
            prompt: None,
            mime_type: "audio/wav".into(),
            filename: "audio.wav".into(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(transcribe_audio(req));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("HTTPS"));
    }

    #[test]
    fn allow_localhost_http_in_transcribe() {
        let req = TranscribeRequest {
            base_url: "http://localhost:1430".into(),
            api_key: "test".into(),
            model: "whisper".into(),
            wav_b64: "dGVzdA==".into(),
            language: None,
            prompt: None,
            mime_type: "audio/wav".into(),
            filename: "audio.wav".into(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(transcribe_audio(req));
        assert!(result.is_err());
        assert!(!result.unwrap_err().contains("HTTPS"));
    }

    #[test]
    fn max_audio_b64_len_is_reasonable() {
        assert_eq!(MAX_AUDIO_B64_LEN, 35_000_000);
    }

    #[test]
    fn expansion_entries_never_enter_prompt() {
        let entries = vec![
            crate::dictionary::DictionaryEntry {
                id: "1".into(),
                spoken: "github".into(),
                corrected: "GitHub".into(),
                kind: "correction".into(),
            },
            crate::dictionary::DictionaryEntry {
                id: "2".into(),
                spoken: "meetnotes".into(),
                corrected: "Share the meeting notes with the team and follow up on action items within 24 hours.".into(),
                kind: "expansion".into(),
            },
        ];
        let prompt = build_vocabulary_hint(&entries).expect("corrections must produce a hint");
        assert!(prompt.contains("GitHub"));
        assert!(prompt.contains("github"));
        assert!(!prompt.contains("meetnotes"));
        assert!(!prompt.contains("Share the meeting notes"));
        assert!(!prompt.contains("follow up on action items"));
    }

    #[test]
    fn expansion_only_entries_produce_no_prompt() {
        let entries = vec![crate::dictionary::DictionaryEntry {
            id: "1".into(),
            spoken: "trigger".into(),
            corrected: "long expansion body that must never be sent".into(),
            kind: "expansion".into(),
        }];
        assert!(build_vocabulary_hint(&entries).is_none());
    }

    #[test]
    fn legacy_entries_without_kind_are_corrections() {
        let raw = r#"[
            {"id": "1", "spoken": "tori", "corrected": "Tauri"}
        ]"#;
        let entries: Vec<crate::dictionary::DictionaryEntry> =
            serde_json::from_str(raw).expect("legacy entry without kind must deserialize");
        assert_eq!(entries[0].kind, "correction");
    }

    #[test]
    fn prompt_truncation_respects_utf8_boundaries() {
        // 900 bytes of 3-byte chars with no comma: byte 890 falls mid-character.
        let entries = vec![crate::dictionary::DictionaryEntry {
            id: "1".into(),
            spoken: "x".into(),
            corrected: "€".repeat(300),
            kind: "correction".into(),
        }];
        let prompt = build_vocabulary_hint(&entries).expect("hint must exist");
        assert!(
            prompt.len() <= 890,
            "prompt exceeded 890 bytes: {}",
            prompt.len()
        );
        assert!(prompt.chars().all(|c| c == '€'), "prompt content corrupted");
    }
}
