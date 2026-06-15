use anyhow::Result;
use cpal::traits::DeviceTrait;
use flacenc::component::BitRepr;
use flacenc::error::Verify;
use once_cell::sync::Lazy;
use std::sync::{
    atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering},
    Arc, Mutex,
};
use tauri::{AppHandle, Emitter};

// Shared recording state
static RECORDING: AtomicBool = AtomicBool::new(false);
static NATIVE_SAMPLE_RATE: AtomicU32 = AtomicU32::new(44100);
static NATIVE_CHANNELS: AtomicU16 = AtomicU16::new(2);

static AUDIO_BUFFER: Lazy<Mutex<Vec<f32>>> = Lazy::new(|| Mutex::new(Vec::new()));

// Global completion channels
static STREAM_READY_TX: Lazy<Mutex<Option<tokio::sync::oneshot::Sender<()>>>> =
    Lazy::new(|| Mutex::new(None));
static STREAM_DONE_RX: Lazy<Mutex<Option<tokio::sync::oneshot::Receiver<()>>>> =
    Lazy::new(|| Mutex::new(None));

/// List available audio input devices
#[tauri::command]
pub fn list_audio_devices() -> Result<Vec<String>, String> {
    #[cfg(target_os = "windows")]
    {
        use cpal::traits::HostTrait;
        let host = cpal::default_host();
        let devices = host.input_devices().map_err(|e| e.to_string())?;
        let names: Vec<String> = devices.filter_map(|d| d.name().ok()).collect();
        Ok(names)
    }
    #[cfg(not(target_os = "windows"))]
    Err("Audio not supported on this platform".to_string())
}

/// Start recording from the microphone
#[tauri::command]
pub async fn start_recording(app: AppHandle, device_id: Option<String>) -> Result<(), String> {
    // If a previous recording is currently stopping/flushing, wait for it to finish
    let start_wait = std::time::Instant::now();
    while RECORDING.load(Ordering::SeqCst) && start_wait.elapsed().as_millis() < 600 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

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
                    .and_then(|mut devs| {
                        devs.find(|d| d.name().ok().as_deref() == Some(id.as_str()))
                    })
                    .or_else(|| host.default_input_device())
            } else {
                host.default_input_device()
            };

            let device = match device {
                Some(d) => d,
                None => {
                    log::error!("No audio input device found");
                    let _ = STREAM_READY_TX
                        .lock()
                        .ok()
                        .and_then(|mut guard| guard.take());
                    return;
                }
            };

            // Query default device configuration to prevent initialization errors
            let default_config = match device.default_input_config() {
                Ok(c) => c,
                Err(e) => {
                    log::error!("Failed to get default input config: {}", e);
                    let _ = STREAM_READY_TX
                        .lock()
                        .ok()
                        .and_then(|mut guard| guard.take());
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

                    // Signal that the stream is ready and actively capturing audio on the very first callback
                    if let Ok(mut guard) = STREAM_READY_TX.lock() {
                        if let Some(tx) = guard.take() {
                            let _ = tx.send(());
                        }
                    }

                    let mut rms_to_emit = None;

                    // Store samples and conditionally compute RMS under a single lock
                    if let Ok(mut buf) = buffer_clone.lock() {
                        // We must record the actual audio immediately so the user's first words aren't dropped!
                        buf.extend_from_slice(data);

                        // Throttle UI updates to ~30fps
                        if let Ok(mut last) = last_emit.try_lock() {
                            if last.elapsed().as_millis() >= 33 {
                                if buf.is_empty() {
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
                            let f32_data: Vec<f32> =
                                data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
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
                            let f32_data: Vec<f32> = data
                                .iter()
                                .map(|&s| (s as f32 - i16::MAX as f32) / i16::MAX as f32)
                                .collect();
                            on_data(&f32_data);
                        },
                        err_fn,
                        None,
                    )
                }
                _ => {
                    log::error!("Unsupported sample format: {:?}", sample_format);
                    RECORDING.store(false, Ordering::SeqCst);
                    let _ = STREAM_READY_TX
                        .lock()
                        .ok()
                        .and_then(|mut guard| guard.take());
                    return;
                }
            };

            match stream {
                Ok(s) => {
                    if s.play().is_ok() {
                        // Wait until recording stops
                        while RECORDING.load(Ordering::SeqCst) {
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        // Copy samples to global buffer
                        if let (Ok(local), Ok(mut global)) = (buffer.lock(), AUDIO_BUFFER.lock()) {
                            *global = local.clone();
                        }
                        let _ = done_tx.send(());
                    } else {
                        log::error!("Failed to play stream");
                        RECORDING.store(false, Ordering::SeqCst);
                        let _ = STREAM_READY_TX
                            .lock()
                            .ok()
                            .and_then(|mut guard| guard.take());
                    }
                }
                Err(e) => {
                    log::error!("Failed to build audio stream: {}", e);
                    RECORDING.store(false, Ordering::SeqCst);
                    let _ = STREAM_READY_TX
                        .lock()
                        .ok()
                        .and_then(|mut guard| guard.take());
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

fn resample_sinc(input: &[f32], from_rate: f64, to_rate: f64) -> Vec<f32> {
    let ratio = to_rate / from_rate;
    if ratio == 1.0 || input.is_empty() {
        return input.to_vec();
    }

    // Cutoff frequency at Nyquist limit of output rate.
    // Apply a guard band (0.92) to maximize high-frequency speech preservation while preventing aliasing.
    let cutoff = 0.92 * (ratio.min(1.0) / 2.0);
    let num_taps = 63; // Reduced to 63 to fix computation latency issue on stopping
    let half_taps = (num_taps / 2) as isize;
    let pad = half_taps as usize;

    if input.len() <= pad {
        return input.to_vec(); // Audio is too short to pad/resample properly
    }

    // Precompute Blackman-Harris window coefficients (92 dB stopband rejection for minimal aliasing)
    let mut window = vec![0.0f64; num_taps];
    for j in 0..num_taps {
        let term1 = 0.35875;
        let term2 =
            0.48829 * (2.0 * std::f64::consts::PI * j as f64 / (num_taps as f64 - 1.0)).cos();
        let term3 =
            0.14128 * (4.0 * std::f64::consts::PI * j as f64 / (num_taps as f64 - 1.0)).cos();
        let term4 =
            0.01168 * (6.0 * std::f64::consts::PI * j as f64 / (num_taps as f64 - 1.0)).cos();
        window[j] = term1 - term2 + term3 - term4;
    }

    // Pad both ends with mirror reflections of the border samples to prevent boundary distortion
    let mut padded = Vec::with_capacity(input.len() + 2 * pad);
    let left_pad: Vec<f32> = input[..pad].iter().rev().copied().collect();
    let right_pad: Vec<f32> = input[input.len() - pad..].iter().rev().copied().collect();

    padded.extend_from_slice(&left_pad);
    padded.extend_from_slice(input);
    padded.extend_from_slice(&right_pad);

    let num_output = (padded.len() as f64 * ratio).round() as usize;
    let mut output = Vec::with_capacity(num_output);

    for i in 0..num_output {
        let center = i as f64 / ratio;
        let mut sum = 0.0f64;
        let mut weight_sum = 0.0f64;

        let center_floor = center.floor() as isize;

        for j in 0..num_taps {
            let tap_idx = center_floor - half_taps + j as isize;
            if tap_idx >= 0 && tap_idx < padded.len() as isize {
                let t = (tap_idx as f64) - center;

                // Sinc function
                let sinc_val = if t == 0.0 {
                    1.0
                } else {
                    let x = 2.0 * std::f64::consts::PI * cutoff * t;
                    x.sin() / x
                };

                // Blackman-Harris window (precomputed)
                let w = window[j];

                let weight = sinc_val * w;
                sum += padded[tap_idx as usize] as f64 * weight;
                weight_sum += weight;
            }
        }

        if weight_sum > 0.0 {
            output.push((sum / weight_sum) as f32);
        } else {
            output.push(0.0);
        }
    }

    // Remove the padded duration from both ends of the output.
    // Each pad corresponds to (pad * ratio) output samples.
    let start_trim = (pad as f64 * ratio).round() as usize;
    let end_trim = (pad as f64 * ratio).round() as usize;

    if output.len() > start_trim + end_trim {
        output[start_trim..output.len() - end_trim].to_vec()
    } else {
        output
    }
}

pub fn create_wav_bytes(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let mut wav = Vec::with_capacity(44 + samples.len() * 2);

    // RIFF Header
    wav.extend_from_slice(b"RIFF");
    let file_size = (36 + samples.len() * 2) as u32;
    wav.extend_from_slice(&file_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");

    // fmt subchunk
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // Subchunk size (16 for PCM)
    wav.extend_from_slice(&1u16.to_le_bytes()); // Audio format (1 = PCM)
    wav.extend_from_slice(&1u16.to_le_bytes()); // Mono (1 channel)
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    let byte_rate = sample_rate * 2; // SampleRate * NumChannels * BitsPerSample/8
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes()); // Block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // Bits per sample (16-bit)

    // data subchunk
    wav.extend_from_slice(b"data");
    let data_size = (samples.len() * 2) as u32;
    wav.extend_from_slice(&data_size.to_le_bytes());

    // PCM samples
    for &sample in samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }

    wav
}

pub fn process_audio_samples(
    samples: Vec<f32>,
    native_sample_rate: u32,
    native_channels: usize,
) -> (Vec<f32>, u32) {
    // 1. Downmix multi-channel stream to mono
    // Extract the primary (first) channel to avoid phase cancellation from averaging
    // stereo/multi-channel recordings. This is the standard approach in voice apps.
    let mono_samples = if native_channels > 1 {
        let mut v = Vec::with_capacity(samples.len() / native_channels);
        for chunk in samples.chunks_exact(native_channels) {
            v.push(chunk[0]);
        }
        v
    } else {
        samples
    };

    // 2. Downsample to 16kHz using windowed-sinc resampler
    const TARGET_SAMPLE_RATE: u32 = 16_000;
    let (resampled, final_sample_rate) = if native_sample_rate != TARGET_SAMPLE_RATE {
        let resampled_data = resample_sinc(
            &mono_samples,
            native_sample_rate as f64,
            TARGET_SAMPLE_RATE as f64,
        );
        (resampled_data, TARGET_SAMPLE_RATE)
    } else {
        (mono_samples, native_sample_rate)
    };

    // 3. DC Offset Removal
    let avg = if !resampled.is_empty() {
        resampled.iter().sum::<f32>() / resampled.len() as f32
    } else {
        0.0f32
    };
    let dc_removed: Vec<f32> = resampled.iter().map(|&s| s - avg).collect();

    // 4. RMS Normalization with Soft Peak Limiting
    // Calculates the Root Mean Square (RMS) of the active signal.
    // Target RMS: 0.18 (ideal level for Whisper ASR).
    let square_sum: f32 = dc_removed.iter().map(|&s| s * s).sum();
    let rms = (square_sum / dc_removed.len() as f32).sqrt();
    let mut normalized = if rms > 0.001 {
        let target_rms = 0.18f32;
        let scale = target_rms / rms;
        dc_removed
            .iter()
            .map(|&s| {
                let scaled = s * scale;
                if scaled.abs() > 0.95 {
                    scaled.signum() * (0.95 + 0.04 * ((scaled.abs() - 0.95) / 0.04).tanh())
                } else {
                    scaled
                }
            })
            .collect::<Vec<f32>>()
    } else {
        dc_removed
    };

    // Append 250ms of silence padding (4,000 zero samples at 16kHz) to give the ASR model
    // enough context to finalize properly without hallucinating words during long silence tails.
    // The 500ms OS buffer sleep already captures trailing syllables.
    normalized.resize(normalized.len() + 4000, 0.0);

    (normalized, final_sample_rate)
}

pub fn process_audio_samples_online(
    samples: Vec<f32>,
    native_sample_rate: u32,
    native_channels: usize,
) -> (Vec<f32>, u32) {
    // 1. Downmix multi-channel stream to mono
    // Extract the primary (first) channel to avoid phase cancellation from averaging
    // stereo/multi-channel recordings. This is the standard approach in voice apps.
    let mono_samples = if native_channels > 1 {
        let mut v = Vec::with_capacity(samples.len() / native_channels);
        for chunk in samples.chunks_exact(native_channels) {
            v.push(chunk[0]);
        }
        v
    } else {
        samples
    };

    // 2. Downsample to 16kHz to prevent payload size limits on long recordings.
    // Whisper natively operates on 16kHz anyway, so this preserves quality
    // while reducing the file size by nearly 3x, lowering latency.
    const TARGET_SAMPLE_RATE: u32 = 16_000;
    let (resampled, final_sample_rate) = if native_sample_rate != TARGET_SAMPLE_RATE {
        let resampled_data = resample_sinc(
            &mono_samples,
            native_sample_rate as f64,
            TARGET_SAMPLE_RATE as f64,
        );
        (resampled_data, TARGET_SAMPLE_RATE)
    } else {
        (mono_samples, native_sample_rate)
    };

    // 3. DC Offset Removal
    let avg = if !resampled.is_empty() {
        resampled.iter().sum::<f32>() / resampled.len() as f32
    } else {
        0.0f32
    };
    let mut dc_removed: Vec<f32> = resampled.iter().map(|&s| s - avg).collect();

    // Append 250ms of silence padding (4,000 zero samples at 16kHz) to give the ASR model
    // enough context to finalize properly without hallucinating words during long silence tails.
    // The 500ms OS buffer sleep already captures trailing syllables.
    dc_removed.resize(dc_removed.len() + 4000, 0.0);

    // Note: We skip the destructive RMS normalization to avoid clipping,
    // and rely on the online API's built-in VAD and AGC.
    (dc_removed, final_sample_rate)
}

fn encode_flac_samples(
    samples: &[f32],
    sample_rate: u32,
    channels: usize,
) -> Result<Vec<u8>, String> {
    if samples.is_empty() {
        return Ok(Vec::new());
    }

    let channels = channels.clamp(1, 8);
    let complete_len = samples.len() - (samples.len() % channels);

    let pcm_samples: Vec<i32> = samples[..complete_len]
        .iter()
        .map(|&s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i32)
        .collect();

    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|e| format!("FLAC config error: {:?}", e))?;

    let source =
        flacenc::source::MemSource::from_samples(&pcm_samples, channels, 16, sample_rate as usize);

    let flac_stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| format!("FLAC encode error: {:?}", e))?;

    let mut sink = flacenc::bitsink::ByteSink::new();
    let _ = flac_stream.write(&mut sink);
    Ok(sink.as_slice().to_vec())
}

pub async fn stop_recording_f32_samples() -> Result<Vec<f32>, String> {
    let start_time = std::time::Instant::now();

    // Give the audio stream 500ms to capture the final spoken syllables from the OS buffer
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

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

    let native_channels = NATIVE_CHANNELS.load(Ordering::SeqCst) as usize;
    let native_sample_rate = NATIVE_SAMPLE_RATE.load(Ordering::SeqCst) as usize;
    if native_channels > 0 && native_sample_rate > 0 {
        let duration_ms = (samples.len() * 1000) / (native_channels * native_sample_rate);
        if duration_ms < 200 {
            log::info!(
                "Recording duration too short ({}ms). Discarding as accidental press.",
                duration_ms
            );
            return Ok(Vec::new());
        }
    }

    let process_start = std::time::Instant::now();

    // Run CPU-intensive audio processing on a dedicated thread to avoid blocking the async executor
    let (processed_samples, final_sample_rate) = tokio::task::spawn_blocking(move || {
        process_audio_samples(samples, native_sample_rate as u32, native_channels)
    })
    .await
    .map_err(|e| e.to_string())?;

    let process_duration = process_start.elapsed();

    log::info!(
        "stop_recording_f32 performance: total = {:?}, wait stream = {:?}, process/resample = {:?}, native_rate = {}Hz -> {}Hz, final size = {} samples",
        start_time.elapsed(),
        wait_duration,
        process_duration,
        native_sample_rate,
        final_sample_rate,
        processed_samples.len()
    );

    Ok(processed_samples)
}

pub async fn stop_recording_wav_bytes() -> Result<Vec<u8>, String> {
    let start_time = std::time::Instant::now();

    // Give the audio stream 500ms to capture the final spoken syllables from the OS buffer
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

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

    let native_channels = NATIVE_CHANNELS.load(Ordering::SeqCst) as usize;
    let native_sample_rate = NATIVE_SAMPLE_RATE.load(Ordering::SeqCst) as usize;
    if native_channels > 0 && native_sample_rate > 0 {
        let duration_ms = (samples.len() * 1000) / (native_channels * native_sample_rate);
        if duration_ms < 200 {
            log::info!(
                "Recording duration too short ({}ms). Discarding as accidental press.",
                duration_ms
            );
            return Ok(Vec::new());
        }
    }

    let process_start = std::time::Instant::now();

    // Run CPU-intensive audio processing on a dedicated thread to avoid blocking the async executor
    let wav_bytes = tokio::task::spawn_blocking(move || {
        let (processed_samples, final_sample_rate) =
            process_audio_samples_online(samples, native_sample_rate as u32, native_channels);

        // Convert samples to i16 for WAV container
        let i16_samples: Vec<i16> = processed_samples
            .iter()
            .map(|&s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .collect();

        // Create WAV bytes (including trailing silence padding to prevent ASR truncation)
        create_wav_bytes(&i16_samples, final_sample_rate)
    })
    .await
    .map_err(|e| e.to_string())?;

    let process_duration = process_start.elapsed();

    log::info!(
        "stop_recording_wav_bytes performance: total = {:?}, wait stream = {:?}, process/wav = {:?}, native_rate = {}Hz, final size = {} bytes",
        start_time.elapsed(),
        wait_duration,
        process_duration,
        native_sample_rate,
        wav_bytes.len()
    );

    Ok(wav_bytes)
}

#[derive(Clone, Debug)]
pub struct AudioPayload {
    pub bytes: Vec<u8>,
    pub mime_type: &'static str,
    pub filename: &'static str,
}

pub async fn stop_recording_flac_bytes() -> Result<Vec<u8>, String> {
    let start_time = std::time::Instant::now();

    // Give the audio stream 500ms to capture the final spoken syllables from the OS buffer
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

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

    let native_channels = NATIVE_CHANNELS.load(Ordering::SeqCst) as usize;
    let native_sample_rate = NATIVE_SAMPLE_RATE.load(Ordering::SeqCst) as usize;
    if native_channels > 0 && native_sample_rate > 0 {
        let duration_ms = (samples.len() * 1000) / (native_channels * native_sample_rate);
        if duration_ms < 200 {
            log::info!(
                "Recording duration too short ({}ms). Discarding as accidental press.",
                duration_ms
            );
            return Ok(Vec::new());
        }
    }

    let process_start = std::time::Instant::now();

    // Resample to 16kHz mono (same as offline path), then lossless FLAC encode.
    // This gives Whisper optimal input at its native training rate and reduces
    // file size ~6x compared to native-rate encoding, lowering upload latency.
    let flac_bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        let (processed, sample_rate) =
            process_audio_samples_online(samples, native_sample_rate as u32, native_channels);
        encode_flac_samples(&processed, sample_rate, 1)
    })
    .await
    .map_err(|e| e.to_string())??;

    let process_duration = process_start.elapsed();

    log::info!(
        "stop_recording_flac_bytes performance: total = {:?}, wait stream = {:?}, process/encode = {:?}, native_rate = {}Hz -> 16000Hz, final size = {} bytes",
        start_time.elapsed(),
        wait_duration,
        process_duration,
        native_sample_rate,
        flac_bytes.len()
    );

    Ok(flac_bytes)
}

pub async fn stop_recording_audio_bytes() -> Result<AudioPayload, String> {
    let settings = crate::settings::load_settings().map_err(|e| e.to_string())?;
    let preset = settings.stt_provider.preset.to_lowercase();

    if preset == "groq" || preset == "openai" || preset == "mistral" {
        let flac_bytes = stop_recording_flac_bytes().await?;
        Ok(AudioPayload {
            bytes: flac_bytes,
            mime_type: "audio/flac",
            filename: "audio.flac",
        })
    } else {
        let wav_bytes = stop_recording_wav_bytes().await?;
        Ok(AudioPayload {
            bytes: wav_bytes,
            mime_type: "audio/wav",
            filename: "audio.wav",
        })
    }
}

/// Stop recording and return the audio file as base64
#[tauri::command]
pub async fn stop_recording() -> Result<String, String> {
    let encode_start = std::time::Instant::now();
    let payload = stop_recording_audio_bytes().await?;
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &payload.bytes);
    log::info!(
        "stop_recording base64 bridge: encode = {:?}, format = {}, size = {} bytes, b64 = {} bytes",
        encode_start.elapsed(),
        payload.filename,
        payload.bytes.len(),
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
    use once_cell::sync::Lazy;
    use std::sync::atomic::Ordering;

    static TEST_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

    #[tokio::test]
    async fn test_resample_perf() {
        let _guard = TEST_LOCK.lock().unwrap();
        let sample_rate = 48000;
        NATIVE_SAMPLE_RATE.store(sample_rate, Ordering::SeqCst);
        NATIVE_CHANNELS.store(2, Ordering::SeqCst);

        let seconds = 40;
        let num_samples = sample_rate as usize * 2 * seconds; // stereo, 40 seconds
        let dummy_samples = vec![0.1f32; num_samples];

        *AUDIO_BUFFER.lock().unwrap() = dummy_samples;

        let start = std::time::Instant::now();
        let bytes = stop_recording_wav_bytes().await.unwrap();
        println!("--- BENCHMARK RESULT: Resample & WAV encode performance test: 40s took {:?}, final size = {} bytes", start.elapsed(), bytes.len());
    }

    #[tokio::test]
    async fn test_flac_encoding_perf() {
        let _guard = TEST_LOCK.lock().unwrap();
        let sample_rate = 48000;
        NATIVE_SAMPLE_RATE.store(sample_rate, Ordering::SeqCst);
        NATIVE_CHANNELS.store(2, Ordering::SeqCst);

        let seconds = 10;
        let num_samples = sample_rate as usize * 2 * seconds; // stereo, 10 seconds
        let dummy_samples = vec![0.1f32; num_samples];

        *AUDIO_BUFFER.lock().unwrap() = dummy_samples;

        let start = std::time::Instant::now();
        let payload = stop_recording_audio_bytes().await.unwrap();

        println!(
            "--- BENCHMARK RESULT: FLAC/WAV encode test: 10s took {:?}, mime = {}, file = {}, size = {} bytes",
            start.elapsed(),
            payload.mime_type,
            payload.filename,
            payload.bytes.len()
        );

        assert!(!payload.bytes.is_empty());

        let settings = crate::settings::load_settings().unwrap();
        let preset = settings.stt_provider.preset.to_lowercase();
        if preset == "groq" || preset == "openai" || preset == "mistral" {
            assert_eq!(payload.mime_type, "audio/flac");
            assert_eq!(payload.filename, "audio.flac");
            assert!(payload.bytes.starts_with(b"fLaC"));
        } else {
            assert_eq!(payload.mime_type, "audio/wav");
            assert_eq!(payload.filename, "audio.wav");
            assert!(payload.bytes.starts_with(b"RIFF"));
        }
    }
}
