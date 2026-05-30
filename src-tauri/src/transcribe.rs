// Fluence Windows — Transcription module
// Sends recorded WAV audio to any OpenAI-compatible /v1/audio/transcriptions endpoint.
// Supports Groq, OpenAI, and custom providers.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use crate::dictionary;

#[derive(Debug, Deserialize)]
struct TranscriptionResponse {
    text: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TranscribeRequest {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub wav_b64: String,    // base64-encoded WAV data
    pub language: Option<String>,
}

/// Transcribe audio via OpenAI-compatible API, then apply dictionary corrections.
#[tauri::command]
pub async fn transcribe_audio(req: TranscribeRequest) -> Result<String, String> {
    let start_time = std::time::Instant::now();
    let mp3_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        &req.wav_b64,
    )
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

pub async fn transcribe_mp3_bytes(
    base_url: &str,
    api_key: &str,
    model: &str,
    mp3_bytes: Vec<u8>,
    language: Option<&str>,
) -> Result<String, String> {
    let start_time = std::time::Instant::now();

    let url = format!(
        "{}/v1/audio/transcriptions",
        base_url.trim_end_matches('/')
    );

    let file_part = reqwest::multipart::Part::bytes(mp3_bytes)
        .file_name("audio.mp3")
        .mime_str("audio/mpeg")
        .map_err(|e| e.to_string())?;

    let mut form = reqwest::multipart::Form::new()
        .part("file", file_part)
        .text("model", model.to_string())
        .text("response_format", "json");

    if let Some(lang) = language {
        if !lang.is_empty() && lang != "auto" {
            form = form.text("language", lang.to_string());
        }
    }

    let network_start = std::time::Instant::now();
    let resp = crate::http_client::CLIENT
        .post(&url)
        .bearer_auth(api_key)
        .multipart(form)
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
        "transcribe_mp3_bytes performance: total = {:?}, network = {:?}, parse/dict = {:?}",
        start_time.elapsed(),
        network_duration,
        parse_start.elapsed()
    );

    Ok(corrected)
}

/// Fetch available models from an OpenAI-compatible /v1/models endpoint.
#[tauri::command]
pub async fn fetch_models(base_url: String, api_key: String) -> Result<Vec<String>, String> {
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));

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
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));

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
        let api_key = match crate::credentials::read_credential(crate::credentials::STT_API_KEY_TARGET) {
            Ok(k) => k,
            Err(e) => {
                println!("--- BENCHMARK RESULT: Skipping STT test because no API key found: {}", e);
                return;
            }
        };

        // Create 5 seconds of dummy mono MP3 audio (sine wave) at 44100Hz
        use shine_rs::{Mp3Encoder, Mp3EncoderConfig, StereoMode};
        let sample_rate = 44100;
        let config = Mp3EncoderConfig::new()
            .sample_rate(sample_rate)
            .bitrate(96)
            .channels(1)
            .stereo_mode(StereoMode::Mono);
        let mut encoder = Mp3Encoder::new(config).unwrap();
        let samples_per_frame = encoder.samples_per_frame();
        let mut mp3_bytes = Vec::new();
        let num_samples = sample_rate as usize * 5;
        let mut dummy_samples = Vec::with_capacity(num_samples);
        for i in 0..num_samples {
            let t = i as f32 / sample_rate as f32;
            let sample = (t * 440.0 * 2.0 * std::f32::consts::PI).sin() * 0.5;
            dummy_samples.push((sample * i16::MAX as f32) as i16);
        }
        
        for chunk in dummy_samples.chunks(samples_per_frame) {
            if chunk.len() == samples_per_frame {
                let mut frames = encoder.encode_interleaved(chunk).unwrap();
                for mut f in frames {
                    mp3_bytes.append(&mut f);
                }
            } else {
                let mut padded = chunk.to_vec();
                padded.resize(samples_per_frame, 0);
                let mut frames = encoder.encode_interleaved(&padded).unwrap();
                for mut f in frames {
                    mp3_bytes.append(&mut f);
                }
            }
        }
        let mut final_frames = encoder.finish().unwrap();
        mp3_bytes.append(&mut final_frames);

        println!("Sending 10 seconds of dummy MP3 audio ({} bytes) to STT API...", mp3_bytes.len());
        let start = std::time::Instant::now();
        let res = transcribe_mp3_bytes(
            &settings.stt_provider.base_url,
            &api_key,
            &settings.stt_provider.model,
            mp3_bytes,
            Some("en"),
        )
        .await;

        match res {
            Ok(text) => {
                println!("--- BENCHMARK RESULT: STT 10s audio transcription successful in {:?}. Response: {:?}", start.elapsed(), text);
            }
            Err(e) => {
                println!("--- BENCHMARK RESULT: STT 10s audio transcription failed in {:?}: {}", start.elapsed(), e);
            }
        }
    }
}

