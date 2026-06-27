// Fluence Windows — Transcription module
// Sends recorded WAV audio to any OpenAI-compatible /v1/audio/transcriptions endpoint.
// Supports Groq, OpenAI, and custom providers.

use crate::dictionary;
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

/// Transcribe audio via an OpenAI-compatible API.
#[tauri::command]
pub async fn transcribe_audio(req: TranscribeRequest) -> Result<String, String> {
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

pub async fn transcribe_mp3_bytes(
    base_url: &str,
    api_key: &str,
    model: &str,
    mp3_bytes: Vec<u8>,
    language: Option<&str>,
) -> Result<String, String> {
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

    // Feed custom dictionary entries to Whisper as an initial prompt vocabulary hint.
    // Groq rejects prompts > 896 characters, so truncate to 890 to be safe.
    if let Ok(entries) = crate::dictionary::get_dictionary() {
        if !entries.is_empty() {
            let mut prompt_words = Vec::new();
            for entry in entries {
                if !entry.corrected.trim().is_empty() {
                    prompt_words.push(entry.corrected.clone());
                }
                if !entry.spoken.trim().is_empty() && entry.spoken != entry.corrected {
                    prompt_words.push(entry.spoken.clone());
                }
            }
            if !prompt_words.is_empty() {
                let mut prompt = prompt_words.join(", ");
                if prompt.len() > 890 {
                    let truncated = &prompt[..890];
                    if let Some(last_comma) = truncated.rfind(',') {
                        prompt = truncated[..last_comma].to_string();
                    } else {
                        prompt = truncated.to_string();
                    }
                    log::debug!(
                        "Prompt truncated to {} characters for Groq compatibility",
                        prompt.len()
                    );
                }
                form = form.text("prompt", prompt);
            }
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

    // Apply custom dictionary corrections
    let corrected = dictionary::apply_corrections(&result.text);

    log::info!(
        "transcribe_mp3_bytes performance: total = {:?}, network = {:?}, parse = {:?}, multipart = {:?}",
        start_time.elapsed(),
        network_duration,
        parse_start.elapsed(),
        multipart_start.elapsed()
    );

    Ok(corrected)
}

/// Fetch available models from an OpenAI-compatible /v1/models endpoint.
#[tauri::command]
pub async fn fetch_models(base_url: String, api_key: String) -> Result<Vec<String>, String> {
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
}
