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
    let api_key = crate::credentials::read_credential(crate::credentials::STT_API_KEY_TARGET)
        .map_err(|_| "No STT API key found".to_string())?;

    let mp3_start = std::time::Instant::now();
    let mp3_bytes = crate::audio::stop_recording_mp3_bytes().await?;
    let mp3_duration = mp3_start.elapsed();

    let transcribe_start = std::time::Instant::now();
    let text = crate::transcribe::transcribe_mp3_bytes(
        &settings.stt_provider.base_url,
        &api_key,
        &settings.stt_provider.model,
        mp3_bytes,
        Some(settings.language.as_str()),
    )
    .await?;
    let transcribe_duration = transcribe_start.elapsed();

    log::info!(
        "stop_and_transcribe workflow: total = {:?}, mp3 = {:?}, transcribe = {:?}",
        start_time.elapsed(),
        mp3_duration,
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

#[tauri::command]
pub async fn finish_transcription_flow(app: tauri::AppHandle) -> Result<TranscriptionFlowResult, String> {
    let start_time = std::time::Instant::now();
    let result = stop_and_transcribe().await?;

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

