use anyhow::Result;
use cpal::traits::DeviceTrait;
use once_cell::sync::Lazy;
use std::sync::{
    atomic::{AtomicBool, Ordering, AtomicU32, AtomicU16},
    Arc, Mutex,
};
use tauri::{AppHandle, Emitter};
use shine_rs::{Mp3Encoder, Mp3EncoderConfig, StereoMode};


// Shared recording state
static RECORDING: AtomicBool = AtomicBool::new(false);
static NATIVE_SAMPLE_RATE: AtomicU32 = AtomicU32::new(44100);
static NATIVE_CHANNELS: AtomicU16 = AtomicU16::new(2);

static AUDIO_BUFFER: Lazy<Mutex<Vec<f32>>> = Lazy::new(|| Mutex::new(Vec::new()));

// Global completion channels
static STREAM_READY_TX: Lazy<Mutex<Option<tokio::sync::oneshot::Sender<()>>>> = Lazy::new(|| Mutex::new(None));
static STREAM_DONE_RX: Lazy<Mutex<Option<tokio::sync::oneshot::Receiver<()>>>> = Lazy::new(|| Mutex::new(None));

/// List available audio input devices
#[tauri::command]
pub fn list_audio_devices() -> Result<Vec<String>, String> {
    #[cfg(target_os = "windows")]
    {
        use cpal::traits::HostTrait;
        let host = cpal::default_host();
        let devices = host
            .input_devices()
            .map_err(|e| e.to_string())?;
        let names: Vec<String> = devices
            .filter_map(|d| d.name().ok())
            .collect();
        Ok(names)
    }
    #[cfg(not(target_os = "windows"))]
    Err("Audio not supported on this platform".to_string())
}

/// Start recording from the microphone
#[tauri::command]
pub async fn start_recording(app: AppHandle, device_id: Option<String>) -> Result<(), String> {
    if RECORDING.load(Ordering::SeqCst) {
        return Err("Already recording".to_string());
    }

    RECORDING.store(true, Ordering::SeqCst);

    // Clear buffer
    if let Ok(mut buf) = AUDIO_BUFFER.lock() {
        buf.clear();
    }

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

    *STREAM_READY_TX.lock().map_err(|e| e.to_string())? = Some(ready_tx);
    *STREAM_DONE_RX.lock().map_err(|e| e.to_string())? = Some(done_rx);

    #[cfg(target_os = "windows")]
    {
        let is_recording = Arc::new(AtomicBool::new(true));
        let is_recording_clone = is_recording.clone();

        tokio::task::spawn_blocking(move || {
            use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

            let host = cpal::default_host();

            // Select device
            let device = if let Some(ref id) = device_id {
                host.input_devices()
                    .ok()
                    .and_then(|mut devs| devs.find(|d| d.name().ok().as_deref() == Some(id.as_str())))
                    .or_else(|| host.default_input_device())
            } else {
                host.default_input_device()
            };

            let device = match device {
                Some(d) => d,
                None => {
                    log::error!("No audio input device found");
                    let _ = STREAM_READY_TX.lock().ok().and_then(|mut guard| guard.take());
                    return;
                }
            };

            // Query default device configuration to prevent initialization errors
            let default_config = match device.default_input_config() {
                Ok(c) => c,
                Err(e) => {
                    log::error!("Failed to get default input config: {}", e);
                    let _ = STREAM_READY_TX.lock().ok().and_then(|mut guard| guard.take());
                    return;
                }
            };

            let config = default_config.config();
            NATIVE_SAMPLE_RATE.store(config.sample_rate.0, Ordering::SeqCst);
            NATIVE_CHANNELS.store(config.channels, Ordering::SeqCst);

            let buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
            let buffer_clone = buffer.clone();
            let app_clone = app.clone();
            let sample_format = default_config.sample_format();
            let err_fn = |err| log::error!("Audio stream error: {}", err);

            let last_emit = Arc::new(Mutex::new(std::time::Instant::now()));
            let on_data_f32 = {
                let is_recording_clone = is_recording_clone.clone();
                let buffer_clone = buffer_clone.clone();
                let app_clone = app_clone.clone();
                let last_emit = last_emit.clone();
                move |data: &[f32]| {
                    if !is_recording_clone.load(Ordering::SeqCst) {
                        return;
                    }

                    let mut rms_to_emit = None;

                    // Store samples and conditionally compute RMS under a single lock
                    if let Ok(mut buf) = buffer_clone.lock() {
                        // Suppress the first ~500ms of interleaved samples so device wake-up
                        // transients do not get recorded or used to calibrate the visualizer.
                        let suppress_samples =
                            (config.sample_rate.0 as usize * config.channels as usize) / 2;
                        let is_early = buf.len() < suppress_samples;

                        // We must record the actual audio even if it's early, otherwise
                        // the user's first words get dropped!
                        buf.extend_from_slice(data);

                        // Throttle UI updates to ~30fps
                        if let Ok(mut last) = last_emit.try_lock() {
                            if last.elapsed().as_millis() >= 33 {
                                if buf.is_empty() {
                                    rms_to_emit = Some(0.0f32);
                                } else {
                                    if is_early {
                                        rms_to_emit = Some(0.0f32);
                                    } else {
                                        // Sliding window of roughly 45ms of interleaved samples.
                                        let window_size = ((config.sample_rate.0 as usize
                                            * config.channels as usize
                                            * 45)
                                            / 1000)
                                            .max(512);
                                        let start = buf.len().saturating_sub(window_size);
                                        let slice = &buf[start..];
                                        let sum_sq: f32 = slice.iter().map(|&s| s * s).sum();
                                        rms_to_emit = Some((sum_sq / slice.len() as f32).sqrt());
                                    }
                                }
                                *last = std::time::Instant::now();
                            }
                        }
                    }

                    // Emit outside the lock — send to overlay window directly
                    if let Some(rms) = rms_to_emit {
                        let amplitude = (rms * 400.0).clamp(0.0, 1.5);
                        // Emit globally so the overlay (and any other windows) receive it
                        let _ = app_clone.emit("audio-amplitude", amplitude);
                    }
                }
            };

            let stream = match sample_format {
                cpal::SampleFormat::F32 => {
                    let on_data = on_data_f32;
                    device.build_input_stream(
                        &config,
                        move |data: &[f32], _| on_data(data),
                        err_fn,
                        None,
                    )
                }
                cpal::SampleFormat::I16 => {
                    let on_data = on_data_f32;
                    device.build_input_stream(
                        &config,
                        move |data: &[i16], _| {
                            let f32_data: Vec<f32> = data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                            on_data(&f32_data);
                        },
                        err_fn,
                        None,
                    )
                }
                cpal::SampleFormat::U16 => {
                    let on_data = on_data_f32;
                    device.build_input_stream(
                        &config,
                        move |data: &[u16], _| {
                            let f32_data: Vec<f32> = data.iter().map(|&s| (s as f32 - i16::MAX as f32) / i16::MAX as f32).collect();
                            on_data(&f32_data);
                        },
                        err_fn,
                        None,
                    )
                }
                _ => {
                    log::error!("Unsupported sample format: {:?}", sample_format);
                    let _ = STREAM_READY_TX.lock().ok().and_then(|mut guard| guard.take());
                    return;
                }
            };

            match stream {
                Ok(s) => {
                    if s.play().is_ok() {
                        // Signal that stream is ready
                        if let Some(tx) = STREAM_READY_TX.lock().ok().and_then(|mut guard| guard.take()) {
                            let _ = tx.send(());
                        }

                        // Wait until recording stops
                        while RECORDING.load(Ordering::SeqCst) {
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        // Copy samples to global buffer
                        if let (Ok(local), Ok(mut global)) =
                            (buffer.lock(), AUDIO_BUFFER.lock())
                        {
                            *global = local.clone();
                        }
                        let _ = done_tx.send(());
                    } else {
                        log::error!("Failed to play stream");
                        let _ = STREAM_READY_TX.lock().ok().and_then(|mut guard| guard.take());
                    }
                }
                Err(e) => {
                    log::error!("Failed to build audio stream: {}", e);
                    let _ = STREAM_READY_TX.lock().ok().and_then(|mut guard| guard.take());
                }
            }
        });

        // Wait up to 3s for stream to be ready
        match tokio::time::timeout(std::time::Duration::from_secs(3), ready_rx).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => {
                RECORDING.store(false, Ordering::SeqCst);
                Err("Audio stream closed unexpectedly".to_string())
            }
            Err(_) => {
                RECORDING.store(false, Ordering::SeqCst);
                Err("Audio device timed out (3s)".to_string())
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err("Audio not supported on this platform".to_string())
    }
}

pub async fn stop_recording_mp3_bytes() -> Result<Vec<u8>, String> {
    let start_time = std::time::Instant::now();

    // Give the audio stream 150ms to capture the final spoken syllables from the OS buffer
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    RECORDING.store(false, Ordering::SeqCst);

    // Wait for the recording task to finish flushing (up to 2 seconds)
    let rx = {
        let mut rx_guard = STREAM_DONE_RX.lock().map_err(|e| e.to_string())?;
        rx_guard.take()
    };

    if let Some(rx) = rx {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), rx).await;
    }
    let wait_duration = start_time.elapsed();

    // Use mem::take() instead of .clone() to avoid a full 7+ MB heap copy.
    let samples = {
        let mut buf = AUDIO_BUFFER.lock().map_err(|e| e.to_string())?;
        std::mem::take(&mut *buf)
    };

    if samples.is_empty() {
        return Err("No audio recorded".to_string());
    }

    let process_start = std::time::Instant::now();

    // Downmix multi-channel stream to mono
    let native_channels = NATIVE_CHANNELS.load(Ordering::SeqCst) as usize;
    let mono_samples: Vec<f32> = if native_channels > 1 {
        let mut v = Vec::with_capacity(samples.len() / native_channels);
        for chunk in samples.chunks_exact(native_channels) {
            let sum: f32 = chunk.iter().sum();
            v.push(sum / native_channels as f32);
        }
        v
    } else {
        samples
    };

    // Downsample to 16kHz using linear interpolation.
    // Whisper natively operates at 16kHz. Sending 48kHz audio wastes 3x upload bandwidth.
    // Linear interpolation is extremely fast (zero CPU overhead), has no group delay, 
    // and avoids the word-skipping tail cutoff artifacts caused by block-based sinc resamplers.
    let native_sample_rate = NATIVE_SAMPLE_RATE.load(Ordering::SeqCst);
    const TARGET_SAMPLE_RATE: u32 = 44_100;

    let (final_samples, final_sample_rate) = if native_sample_rate != TARGET_SAMPLE_RATE {
        let from_rate = native_sample_rate as f64;
        let to_rate = TARGET_SAMPLE_RATE as f64;
        let ratio = from_rate / to_rate;

        let num_output_samples = (mono_samples.len() as f64 / ratio).round() as usize;
        let mut resampled = Vec::with_capacity(num_output_samples);

        for i in 0..num_output_samples {
            let src_index = i as f64 * ratio;
            let index_floor = src_index.floor() as usize;
            let index_ceil = (index_floor + 1).min(mono_samples.len() - 1);
            let t = (src_index - index_floor as f64) as f32;

            if index_floor < mono_samples.len() {
                let sample = (1.0 - t) * mono_samples[index_floor] + t * mono_samples[index_ceil];
                resampled.push(sample);
            }
        }

        (resampled, TARGET_SAMPLE_RATE)
    } else {
        // Audio is already at 16kHz (unlikely but handle gracefully)
        (mono_samples, native_sample_rate)
    };

    // Volume Normalization: Scale the audio so that the peak absolute sample is 0.9.
    // This boosts quiet microphone inputs, preventing Whisper accuracy loss or hallucinations.
    let peak = final_samples.iter().map(|&s| s.abs()).fold(0.0f32, f32::max);
    let normalized_samples = if peak > 0.01 && peak < 0.8 {
        let scale = 0.9 / peak;
        final_samples.iter().map(|&s| s * scale).collect::<Vec<f32>>()
    } else {
        final_samples
    };

    // Convert samples to i16 for MP3 encoding
    let i16_samples: Vec<i16> = normalized_samples
        .iter()
        .map(|&s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
        .collect();

    // Encode to MP3 using shine-rs
    let config = Mp3EncoderConfig::new()
        .sample_rate(final_sample_rate)
        .bitrate(96)
        .channels(1)
        .stereo_mode(StereoMode::Mono);

    let mut encoder = Mp3Encoder::new(config).map_err(|e| format!("Failed to create MP3 encoder: {:?}", e))?;
    let samples_per_frame = encoder.samples_per_frame();
    let mut mp3_bytes = Vec::new();

    for chunk in i16_samples.chunks(samples_per_frame) {
        if chunk.len() == samples_per_frame {
            let mut frames = encoder.encode_interleaved(chunk).map_err(|e| format!("MP3 encoding error: {:?}", e))?;
            for mut f in frames {
                mp3_bytes.append(&mut f);
            }
        } else {
            let mut padded = chunk.to_vec();
            padded.resize(samples_per_frame, 0);
            let mut frames = encoder.encode_interleaved(&padded).map_err(|e| format!("MP3 encoding error: {:?}", e))?;
            for mut f in frames {
                mp3_bytes.append(&mut f);
            }
        }
    }

    let mut final_frames = encoder.finish().map_err(|e| format!("Failed to finalize MP3 encoder: {:?}", e))?;
    mp3_bytes.append(&mut final_frames);

    let process_duration = process_start.elapsed();

    log::info!(
        "stop_recording performance: total = {:?}, wait stream = {:?}, process/resample/mp3 = {:?}, native_rate = {}Hz -> {}Hz, final size = {} bytes",
        start_time.elapsed(),
        wait_duration,
        process_duration,
        native_sample_rate,
        final_sample_rate,
        mp3_bytes.len()
    );

    Ok(mp3_bytes)
}

/// Stop recording and return the MP3 file as base64
#[tauri::command]
pub async fn stop_recording() -> Result<String, String> {
    let encode_start = std::time::Instant::now();
    let mp3_bytes = stop_recording_mp3_bytes().await?;
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &mp3_bytes);
    log::info!(
        "stop_recording base64 bridge: encode = {:?}, mp3 = {} bytes, b64 = {} bytes",
        encode_start.elapsed(),
        mp3_bytes.len(),
        b64.len()
    );
    Ok(b64)
}

/// Check if currently recording
#[tauri::command]
pub fn is_recording() -> bool {
    RECORDING.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn test_resample_perf() {
        let sample_rate = 48000;
        NATIVE_SAMPLE_RATE.store(sample_rate, Ordering::SeqCst);
        NATIVE_CHANNELS.store(2, Ordering::SeqCst);
        
        let seconds = 40;
        let num_samples = sample_rate as usize * 2 * seconds; // stereo, 40 seconds
        let dummy_samples = vec![0.1f32; num_samples];
        
        *AUDIO_BUFFER.lock().unwrap() = dummy_samples;
        
        let start = std::time::Instant::now();
        let bytes = stop_recording_mp3_bytes().await.unwrap();
        println!("--- BENCHMARK RESULT: Resample & MP3 encode performance test: 40s took {:?}, final size = {} bytes", start.elapsed(), bytes.len());
    }
}

