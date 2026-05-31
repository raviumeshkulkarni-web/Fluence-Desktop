use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionFlowResult {
    pub text: String,
    pub duration_ms: u64,
    pub provider: String,
}

async fn stop_and_transcribe() -> Result<TranscriptionFlowResult, String> {
    let start_time = std::time::Instant::now();

    let settings = crate::settings::load_settings().map_err(|e| e.to_string())?;
    
    let (text, transcribe_duration, _step_duration) = if settings.stt_provider.preset == "Local Offline" {
        let transcribe_start = std::time::Instant::now();
        let samples = crate::audio::stop_recording_f32_samples().await?;
        let result = crate::offline_transcribe::transcribe_samples(samples)
            .await
            .map_err(|e| format!("Offline transcription error: {}", e))?;
        
        let corrected = crate::dictionary::apply_corrections(&result);
        let elapsed = transcribe_start.elapsed();
        (corrected, elapsed, elapsed)
    } else {
        let api_key = crate::credentials::read_credential(crate::credentials::STT_API_KEY_TARGET)
            .map_err(|_| "No STT API key found. Please configure an API key in Providers settings.".to_string())?;

        let mp3_start = std::time::Instant::now();
        let mp3_bytes = crate::audio::stop_recording_mp3_bytes().await?;
        let mp3_duration = mp3_start.elapsed();

        let transcribe_start = std::time::Instant::now();
        let corrected = crate::transcribe::transcribe_mp3_bytes(
            &settings.stt_provider.base_url,
            &api_key,
            &settings.stt_provider.model,
            mp3_bytes,
            Some(settings.language.as_str()),
        )
        .await?;
        (corrected, transcribe_start.elapsed(), mp3_duration)
    };

    log::info!(
        "stop_and_transcribe workflow: total = {:?}, ASR duration = {:?}",
        start_time.elapsed(),
        transcribe_duration
    );

    Ok(TranscriptionFlowResult {
        text,
        duration_ms: start_time.elapsed().as_millis() as u64,
        provider: settings.stt_provider.preset,
    })
}

#[tauri::command]
pub async fn stop_and_transcribe_recording() -> Result<TranscriptionFlowResult, String> {
    stop_and_transcribe().await
}

async fn polish_transcribed_text(
    base_url: &str,
    api_key: &str,
    model: &str,
    raw_text: &str,
    style: &str,
) -> Result<String, String> {
    let system_prompt = match style {
        "clean" => "You are an AI assistant that cleans up transcription voice dictation. Remove filler words (like um, ah, uh, etc.), correct minor grammar mistakes, and ensure smooth flow. Keep all original sentences and words intact otherwise. Output ONLY the cleaned up text, no explanations, no markdown formatting.",
        "professional" => "You are a professional writing assistant. Rewrite the following dictation text in a professional, formal business tone. Make sure it sounds natural, clear, and polite, suitable for emails and work. Output ONLY the rewritten text, no explanations, no markdown formatting.",
        "bullet_points" => "You are a writing assistant. Convert the following dictation text into a clean, concise bulleted list. Output ONLY the bulleted list.",
        "translate_en" => "You are a translator. Translate the following text into clear, fluent English. Output ONLY the translated English text.",
        _ => return Ok(raw_text.to_string()),
    };

    let url = format!(
        "{}/v1/chat/completions",
        base_url.trim_end_matches('/')
    );

    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": raw_text}
        ],
        "temperature": 0.3,
        "max_tokens": 1024,
    });

    let mut request = crate::http_client::CLIENT
        .post(&url)
        .timeout(std::time::Duration::from_secs(15))
        .json(&body);

    if !api_key.trim().is_empty() {
        request = request.bearer_auth(api_key);
    }

    let resp = request
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("LLM API error {}: {}", status, text));
    }

    #[derive(serde::Deserialize)]
    struct Choice {
        message: Message,
    }
    #[derive(serde::Deserialize)]
    struct Message {
        content: String,
    }
    #[derive(serde::Deserialize)]
    struct ChatResp {
        choices: Vec<Choice>,
    }

    let chat_resp: ChatResp = resp
        .json()
        .await
        .map_err(|e| format!("JSON parse error: {}", e))?;

    let content = chat_resp
        .choices
        .into_iter()
        .next()
        .map(|c| c.message.content)
        .ok_or_else(|| "Empty response from LLM".to_string())?;

    Ok(content.trim().to_string())
}

#[tauri::command]
pub async fn finish_transcription_flow(app: tauri::AppHandle) -> Result<TranscriptionFlowResult, String> {
    let start_time = std::time::Instant::now();
    let mut result = stop_and_transcribe().await?;

    let settings = crate::settings::load_settings().map_err(|e| e.to_string())?;
    
    if settings.ai_polish_style != "none" {
        let llm_key = crate::credentials::read_credential("Fluence/LLM_ApiKey").unwrap_or_default();
        match polish_transcribed_text(
            &settings.llm_provider.base_url,
            &llm_key,
            &settings.llm_provider.model,
            &result.text,
            &settings.ai_polish_style,
        )
        .await
        {
            Ok(polished) => {
                log::info!("AI polished dictation from '{}' to '{}'", result.text, polished);
                result.text = polished;
            }
            Err(e) => {
                log::warn!("AI polish failed: {}, pasting raw transcription instead", e);
            }
        }
    }

    // Hide the overlay BEFORE we inject text so that the underlying app regains focus.
    let _ = crate::overlay::hide_overlay(app.clone());
    
    // Give the OS a moment to process the focus change. 50ms is sufficient on Windows.
    // Previously 150ms — reduced to cut 100ms of unnecessary latency from every paste.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let inject_start = std::time::Instant::now();
    crate::clipboard::inject_text(result.text.clone()).await?;
    let inject_duration = inject_start.elapsed();

    let history_text = result.text.clone();
    let history_provider = result.provider.clone();
    let history_duration_ms = result.duration_ms;
    tokio::spawn(async move {
        if let Err(e) = crate::history::add_history_entry(
            &history_text,
            "transcription",
            history_duration_ms,
            &history_provider,
        ) {
            log::warn!("Failed to save transcription history entry: {}", e);
        }
    });

    log::info!(
        "finish_transcription_flow workflow: total = {:?}, inject = {:?}",
        start_time.elapsed(),
        inject_duration
    );

    Ok(result)
}

