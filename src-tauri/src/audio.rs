use anyhow::Result;
use cpal::traits::DeviceTrait;
use flacenc::component::BitRepr;
use flacenc::error::Verify;
use once_cell::sync::Lazy;
use shine_rs::{Mp3Encoder, Mp3EncoderConfig, StereoMode};
use std::cell::Cell;
use std::sync::{
    atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering},
    Arc, Mutex,
};
use tauri::{AppHandle, Emitter};

// Shared recording state
static RECORDING: AtomicBool = AtomicBool::new(false);
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);
static STARTUP_CANCELLED: AtomicBool = AtomicBool::new(false);
static CALLBACKS_POST_STOP: AtomicU32 = AtomicU32::new(0);
static NATIVE_SAMPLE_RATE: AtomicU32 = AtomicU32::new(44100);
static NATIVE_CHANNELS: AtomicU16 = AtomicU16::new(2);

static AUDIO_BUFFER: Lazy<Mutex<Vec<f32>>> = Lazy::new(|| Mutex::new(Vec::new()));

// Diagnostics timing statics
static TIMING_STOP_REQUESTED: Lazy<Mutex<Option<std::time::Instant>>> =
    Lazy::new(|| Mutex::new(None));
static TIMING_LAST_CALLBACK: Lazy<Mutex<Option<std::time::Instant>>> =
    Lazy::new(|| Mutex::new(None));

// Per-thread flag: tracks whether the current audio callback thread has been promoted to MMCSS priority
thread_local! {
    static THREAD_PROMOTED: Cell<bool> = const { Cell::new(false) };
}

// Global completion channels
static STREAM_READY_TX: Lazy<Mutex<Option<tokio::sync::oneshot::Sender<()>>>> =
    Lazy::new(|| Mutex::new(None));
static STREAM_DONE_RX: Lazy<Mutex<Option<tokio::sync::oneshot::Receiver<()>>>> =
    Lazy::new(|| Mutex::new(None));

fn clear_cancelled_startup_owner() {
    if STARTUP_CANCELLED.swap(false, Ordering::SeqCst) {
        crate::hotkey::clear_active_recording_owner();
    }
}

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
    while RECORDING.load(Ordering::SeqCst) && start_wait.elapsed().as_millis() < 100 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    if RECORDING.load(Ordering::SeqCst) {
        return Err("Already recording".to_string());
    }

    RECORDING.store(true, Ordering::SeqCst);
    let _ = crate::tray::refresh_menu(&app);
    STOP_REQUESTED.store(false, Ordering::SeqCst);
    STARTUP_CANCELLED.store(false, Ordering::SeqCst);
    CALLBACKS_POST_STOP.store(0, Ordering::SeqCst);

    if let Ok(mut guard) = TIMING_STOP_REQUESTED.lock() {
        *guard = None;
    }
    if let Ok(mut guard) = TIMING_LAST_CALLBACK.lock() {
        *guard = None;
    }

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
                    RECORDING.store(false, Ordering::SeqCst);
                    let _ = crate::tray::refresh_menu(&app);
                    clear_cancelled_startup_owner();
                    let _ = STREAM_READY_TX
                        .lock()
                        .ok()
                        .and_then(|mut guard| guard.take());
                    let _ = done_tx.send(());
                    return;
                }
            };

            // Query default device configuration to prevent initialization errors
            let default_config = match device.default_input_config() {
                Ok(c) => c,
                Err(e) => {
                    log::error!("Failed to get default input config: {}", e);
                    RECORDING.store(false, Ordering::SeqCst);
                    let _ = crate::tray::refresh_menu(&app);
                    clear_cancelled_startup_owner();
                    let _ = STREAM_READY_TX
                        .lock()
                        .ok()
                        .and_then(|mut guard| guard.take());
                    let _ = done_tx.send(());
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
                    // Per-thread MMCSS real-time priority promotion (first thing)
                    if !THREAD_PROMOTED.with(|p| p.replace(true)) {
                        let sr = NATIVE_SAMPLE_RATE.load(Ordering::Relaxed);
                        if sr > 0 {
                            match audio_thread_priority::promote_current_thread_to_real_time(0, sr)
                            {
                                Ok(_) => {
                                    log::info!("Audio thread promoted to MMCSS real-time priority")
                                }
                                Err(e) => log::warn!("Failed to promote audio thread: {e}"),
                            }
                        }
                    }

                    if !is_recording_clone.load(Ordering::SeqCst) {
                        return;
                    }

                    if let Ok(mut guard) = TIMING_LAST_CALLBACK.lock() {
                        *guard = Some(std::time::Instant::now());
                    }
                    if STOP_REQUESTED.load(Ordering::SeqCst) {
                        CALLBACKS_POST_STOP.fetch_add(1, Ordering::SeqCst);
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
                        let db = (rms.max(1e-6) as f64).log10() * 20.0;
                        let amplitude = ((db + 75.0) / 45.0).clamp(0.0, 1.0) as f32;
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
                    let _ = crate::tray::refresh_menu(&app);
                    clear_cancelled_startup_owner();
                    let _ = STREAM_READY_TX
                        .lock()
                        .ok()
                        .and_then(|mut guard| guard.take());
                    let _ = done_tx.send(());
                    return;
                }
            };

            match stream {
                Ok(s) => {
                    if s.play().is_ok() {
                        // Wait until stop is requested
                        while !STOP_REQUESTED.load(Ordering::SeqCst) {
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }

                        // Bounded drain: wait until 2 post-stop callbacks arrive (fast path,
                        // typically ~20-40ms) or a hard 200ms timeout is reached (safety net).
                        let drain_start = std::time::Instant::now();

                        while drain_start.elapsed().as_millis() < 200 {
                            if CALLBACKS_POST_STOP.load(Ordering::SeqCst) >= 2 {
                                break;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }

                        let drain_duration = drain_start.elapsed();
                        log::info!(
                            "Drain completed in {:?} (callbacks captured: {})",
                            drain_duration,
                            CALLBACKS_POST_STOP.load(Ordering::SeqCst)
                        );

                        // Stop accepting new callbacks
                        is_recording.store(false, Ordering::SeqCst);

                        // Copy samples to global buffer under lock
                        if let (Ok(local), Ok(mut global)) = (buffer.lock(), AUDIO_BUFFER.lock()) {
                            *global = local.clone();
                        }

                        // Explicitly drop stream to quiesce WASAPI before signaling completion
                        drop(s);

                        // Set RECORDING to false and notify stop commands
                        RECORDING.store(false, Ordering::SeqCst);
                        let _ = crate::tray::refresh_menu(&app);
                        clear_cancelled_startup_owner();
                        let _ = done_tx.send(());
                    } else {
                        log::error!("Failed to play stream");
                        RECORDING.store(false, Ordering::SeqCst);
                        let _ = crate::tray::refresh_menu(&app);
                        clear_cancelled_startup_owner();
                        let _ = STREAM_READY_TX
                            .lock()
                            .ok()
                            .and_then(|mut guard| guard.take());
                        let _ = done_tx.send(());
                    }
                }
                Err(e) => {
                    log::error!("Failed to build audio stream: {}", e);
                    RECORDING.store(false, Ordering::SeqCst);
                    let _ = crate::tray::refresh_menu(&app);
                    clear_cancelled_startup_owner();
                    let _ = STREAM_READY_TX
                        .lock()
                        .ok()
                        .and_then(|mut guard| guard.take());
                    let _ = done_tx.send(());
                }
            }
        });

        // Wait up to 3s for stream to be ready
        match tokio::time::timeout(std::time::Duration::from_secs(3), ready_rx).await {
            Ok(Ok(())) => {
                // Stream is live and capturing — duck background apps if enabled.
                let s = crate::settings::load_settings().unwrap_or_default();
                if s.duck_enabled {
                    crate::ducking::duck(s.duck_level);
                }
                Ok(())
            }
            Ok(Err(_)) => {
                STARTUP_CANCELLED.store(true, Ordering::SeqCst);
                STOP_REQUESTED.store(true, Ordering::SeqCst);
                let done_rx = STREAM_DONE_RX.lock().map_err(|e| e.to_string())?.take();
                if let Some(done_rx) = done_rx {
                    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), done_rx).await;
                }
                clear_cancelled_startup_owner();
                Err("Audio stream closed unexpectedly".to_string())
            }
            Err(_) => {
                STARTUP_CANCELLED.store(true, Ordering::SeqCst);
                STOP_REQUESTED.store(true, Ordering::SeqCst);
                let done_rx = STREAM_DONE_RX.lock().map_err(|e| e.to_string())?.take();
                if let Some(done_rx) = done_rx {
                    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), done_rx).await;
                }
                clear_cancelled_startup_owner();
                Err("Audio device timed out (3s)".to_string())
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err("Audio not supported on this platform".to_string())
    }
}

fn resample_fast_voice(input: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || input.is_empty() {
        return input.to_vec();
    }

    let ratio = from_rate as f64 / to_rate as f64;
    let out_len = (input.len() as f64 / ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);

    // Hann-windowed Sinc FIR Filter (anti-aliasing)
    // 31 taps provides a good balance between anti-aliasing strength and performance.
    // The Hann window ensures there is no "metallic ringing" (Gibbs phenomenon) while
    // still maintaining a steep roll-off to block high frequencies that confuse Whisper.
    let num_taps = 31;
    let mut filter = vec![0.0f32; num_taps];
    let half_taps = (num_taps / 2) as isize;

    // Cutoff frequency at 90% of the Nyquist of the target sample rate
    // (e.g., for 16kHz target, Nyquist is 8kHz, cutoff is 7.2kHz)
    let cutoff_freq = (to_rate as f64 / 2.0) * 0.9;
    let normalized_cutoff = cutoff_freq / from_rate as f64;

    let mut sum_taps = 0.0;
    for i in 0..num_taps {
        let n = i as isize - half_taps;

        let sinc = if n == 0 {
            2.0 * normalized_cutoff
        } else {
            let x = 2.0 * std::f64::consts::PI * normalized_cutoff * (n as f64);
            (x.sin() / x) * (2.0 * normalized_cutoff)
        };

        // Hann window formula: 0.5 * (1 - cos(2*PI*i / (N-1)))
        let window =
            0.5 * (1.0 - (2.0 * std::f64::consts::PI * (i as f64) / ((num_taps - 1) as f64)).cos());

        filter[i] = (sinc * window) as f32;
        sum_taps += filter[i];
    }

    // Normalize filter coefficients so they sum to 1.0 (0 dB DC gain)
    for i in 0..num_taps {
        filter[i] /= sum_taps;
    }

    // Convolution and decimation
    for i in 0..out_len {
        let center_idx = (i as f64 * ratio).round() as isize;
        let mut sample = 0.0;

        for (j, &coeff) in filter.iter().enumerate() {
            let input_idx = center_idx + j as isize - half_taps;
            if input_idx >= 0 && input_idx < input.len() as isize {
                sample += input[input_idx as usize] * coeff;
            }
        }

        out.push(sample);
    }

    out
}

#[allow(dead_code)]
pub fn create_wav_bytes(samples: &[i16], sample_rate: u32, channels: u16) -> Vec<u8> {
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
    wav.extend_from_slice(&channels.to_le_bytes()); // Channel count
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    let byte_rate = sample_rate * channels as u32 * 2; // SampleRate * NumChannels * BitsPerSample/8
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    let block_align = channels * 2; // NumChannels * BitsPerSample/8
    wav.extend_from_slice(&block_align.to_le_bytes()); // Block align
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
    // 1. Downmix multi-channel stream to mono by averaging all channels.
    let mono_samples = if native_channels > 1 {
        let mut v = Vec::with_capacity(samples.len() / native_channels);
        for chunk in samples.chunks_exact(native_channels) {
            let sum: f32 = chunk.iter().sum();
            v.push(sum / native_channels as f32);
        }
        v
    } else {
        samples
    };

    // 2. Downsample to 16kHz using Boxcar (Moving Average) Resampler
    // This perfectly preserves high-frequency crispness without metallic ringing.
    const TARGET_SAMPLE_RATE: u32 = 16_000;
    let (resampled, final_sample_rate) = if native_sample_rate != TARGET_SAMPLE_RATE {
        let resampled_data =
            resample_fast_voice(&mono_samples, native_sample_rate, TARGET_SAMPLE_RATE);
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

    // 4. Peak Normalization (match online pipeline)
    // Boosts quiet microphone inputs so sherpa-onnx can detect speech.
    let peak = dc_removed.iter().map(|&s| s.abs()).fold(0.0f32, f32::max);
    let normalized = if peak > 0.005 && peak < 0.98 {
        let scale = 0.9 / peak;
        dc_removed.iter().map(|&s| s * scale).collect::<Vec<f32>>()
    } else {
        dc_removed
    };

    // Note: Silence padding removed to prevent artificial tail clipping by VAD.
    // The OS buffer sleep in the stop logic already captures trailing syllables.

    (normalized, final_sample_rate)
}

pub fn process_audio_samples_online(
    samples: Vec<f32>,
    native_sample_rate: u32,
    native_channels: usize,
) -> (Vec<f32>, u32) {
    // 1. Mono Downmixing (v1.0.0 Channel Averaging)
    let _t_start = std::time::Instant::now();
    let mono_samples = if native_channels > 1 {
        let mut v = Vec::with_capacity(samples.len() / native_channels);
        for chunk in samples.chunks_exact(native_channels) {
            let sum: f32 = chunk.iter().sum();
            v.push(sum / native_channels as f32);
        }
        v
    } else {
        samples
    };

    // 2. Client-Side Resampling to 16kHz (v1.0.0 Box Filter)
    let _t_start = std::time::Instant::now();
    const TARGET_SAMPLE_RATE: u32 = 16_000;
    let (resampled, final_sample_rate) = if native_sample_rate != TARGET_SAMPLE_RATE {
        let from_rate = native_sample_rate as f64;
        let to_rate = TARGET_SAMPLE_RATE as f64;
        let ratio = from_rate / to_rate;

        let num_output_samples = (mono_samples.len() as f64 / ratio).round() as usize;
        let mut v = Vec::with_capacity(num_output_samples);

        for i in 0..num_output_samples {
            let src_center = i as f64 * ratio;
            let start = (src_center - ratio / 2.0).max(0.0);
            let end = (src_center + ratio / 2.0).min((mono_samples.len() - 1) as f64);

            let start_idx = start.floor() as usize;
            let end_idx = end.ceil() as usize;

            let mut sum = 0.0f32;
            let mut count = 0.0f32;

            for idx in start_idx..=end_idx {
                let sample_start = idx as f64 - 0.5;
                let sample_end = idx as f64 + 0.5;

                let overlap_start = sample_start.max(start);
                let overlap_end = sample_end.min(end);

                if overlap_end > overlap_start {
                    let weight = (overlap_end - overlap_start) as f32;
                    sum += mono_samples[idx] * weight;
                    count += weight;
                }
            }

            if count > 0.0 {
                v.push(sum / count);
            } else {
                v.push(0.0);
            }
        }
        (v, TARGET_SAMPLE_RATE)
    } else {
        (mono_samples, native_sample_rate)
    };

    // 3. DC Offset Removal
    let _t_start = std::time::Instant::now();
    let avg = if !resampled.is_empty() {
        resampled.iter().sum::<f32>() / resampled.len() as f32
    } else {
        0.0f32
    };
    let dc_removed: Vec<f32> = resampled.iter().map(|&s| s - avg).collect();

    // 4. Volume Normalization (v1.0.0 Peak Normalization to 0.9)
    let _t_start = std::time::Instant::now();
    let peak = dc_removed.iter().map(|&s| s.abs()).fold(0.0f32, f32::max);
    let mut normalized = if peak > 0.005 && peak < 0.98 {
        let scale = 0.9 / peak;
        dc_removed.iter().map(|&s| s * scale).collect::<Vec<f32>>()
    } else {
        dc_removed
    };

    // 5. Silence Padding (v1.0.0 500ms Trailing Padding)
    let _t_start = std::time::Instant::now();
    let padding_size = (final_sample_rate as usize) / 2;
    normalized.resize(normalized.len() + padding_size, 0.0);

    (normalized, final_sample_rate)
}

#[allow(dead_code)]
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
    // Un-duck background apps on every exit path (errors, early returns) via RAII.
    let _restore_guard = crate::ducking::restore_guard();
    let _recording_owner_guard = crate::hotkey::RecordingStopGuard;
    // Match the online paths so the final syllable is present before shutdown.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let stop_request_time = std::time::Instant::now();
    if let Ok(mut guard) = TIMING_STOP_REQUESTED.lock() {
        *guard = Some(stop_request_time);
    }
    // Reset counter BEFORE signaling stop so callbacks that fire after
    // STOP_REQUESTED is set are counted from a clean baseline.
    CALLBACKS_POST_STOP.store(0, Ordering::SeqCst);
    STOP_REQUESTED.store(true, Ordering::SeqCst);

    // Wait for the recording task to finish flushing (up to 2 seconds)
    let rx = {
        let mut rx_guard = STREAM_DONE_RX.lock().map_err(|e| e.to_string())?;
        rx_guard.take()
    };

    if let Some(rx) = rx {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), rx).await;
    }
    let wait_duration = stop_request_time.elapsed();
    let encoding_start_time = std::time::Instant::now();

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
        if duration_ms > 600_000 {
            return Err(format!(
                "Recording too long ({}ms, {:.1} minutes). Maximum is 10 minutes. Please split recordings.",
                duration_ms,
                duration_ms as f64 / 60_000.0
            ));
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

    let last_cb_time = TIMING_LAST_CALLBACK.lock().ok().and_then(|g| *g);
    let cb_delay = last_cb_time
        .map(|cb| {
            if cb > stop_request_time {
                cb.duration_since(stop_request_time).as_millis()
            } else {
                0
            }
        })
        .unwrap_or(0);

    log::info!(
        "Stop lifecycle diagnostics (f32): total wait = {:?}, last callback arrived {} ms after stop request, processing started {} ms after stop request",
        stop_request_time.elapsed(),
        cb_delay,
        encoding_start_time.duration_since(stop_request_time).as_millis()
    );

    log::info!(
        "stop_recording_f32 performance: total = {:?}, wait stream = {:?}, process/resample = {:?}, native_rate = {}Hz -> {}Hz, final size = {} samples",
        stop_request_time.elapsed(),
        wait_duration,
        process_duration,
        native_sample_rate,
        final_sample_rate,
        processed_samples.len()
    );

    Ok(processed_samples)
}

#[allow(dead_code)]
pub async fn stop_recording_wav_bytes() -> Result<Vec<u8>, String> {
    let _recording_owner_guard = crate::hotkey::RecordingStopGuard;
    // Give the microphone stream 100ms of continued recording after the hotkey release
    // before signalling stop. This matches v1.1.20's approach that eliminated truncation:
    // the final syllable lands in the buffer while the stream is still fully active.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let stop_request_time = std::time::Instant::now();
    if let Ok(mut guard) = TIMING_STOP_REQUESTED.lock() {
        *guard = Some(stop_request_time);
    }
    // Reset counter BEFORE signaling stop so callbacks that fire after
    // STOP_REQUESTED is set are counted from a clean baseline.
    CALLBACKS_POST_STOP.store(0, Ordering::SeqCst);
    STOP_REQUESTED.store(true, Ordering::SeqCst);

    // Wait for the recording task to finish flushing (up to 2 seconds)
    let rx = {
        let mut rx_guard = STREAM_DONE_RX.lock().map_err(|e| e.to_string())?;
        rx_guard.take()
    };

    if let Some(rx) = rx {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), rx).await;
    }
    let wait_duration = stop_request_time.elapsed();
    let encoding_start_time = std::time::Instant::now();

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
        if duration_ms > 600_000 {
            return Err(format!(
                "Recording too long ({}ms, {:.1} minutes). Maximum is 10 minutes. Please split recordings.",
                duration_ms,
                duration_ms as f64 / 60_000.0
            ));
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

        // Create WAV bytes (mono layout after online processing)
        create_wav_bytes(&i16_samples, final_sample_rate, 1)
    })
    .await
    .map_err(|e| e.to_string())?;

    let process_duration = process_start.elapsed();

    let last_cb_time = TIMING_LAST_CALLBACK.lock().ok().and_then(|g| *g);
    let cb_delay = last_cb_time
        .map(|cb| {
            if cb > stop_request_time {
                cb.duration_since(stop_request_time).as_millis()
            } else {
                0
            }
        })
        .unwrap_or(0);

    log::info!(
        "Stop lifecycle diagnostics (wav): total wait = {:?}, last callback arrived {} ms after stop request, processing started {} ms after stop request",
        stop_request_time.elapsed(),
        cb_delay,
        encoding_start_time.duration_since(stop_request_time).as_millis()
    );

    log::info!(
        "stop_recording_wav_bytes performance: total = {:?}, wait stream = {:?}, process/wav = {:?}, rate = {}Hz, final size = {} bytes",
        stop_request_time.elapsed(),
        wait_duration,
        process_duration,
        native_sample_rate,
        wav_bytes.len()
    );

    Ok(wav_bytes)
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct AudioPayload {
    pub bytes: Vec<u8>,
    pub mime_type: &'static str,
    pub filename: &'static str,
}

#[allow(dead_code)]
pub async fn stop_recording_flac_bytes() -> Result<Vec<u8>, String> {
    let _recording_owner_guard = crate::hotkey::RecordingStopGuard;
    // Give the microphone stream 100ms of continued recording after the hotkey release
    // before signalling stop. This matches v1.1.20's approach that eliminated truncation:
    // the final syllable lands in the buffer while the stream is still fully active.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let stop_request_time = std::time::Instant::now();
    if let Ok(mut guard) = TIMING_STOP_REQUESTED.lock() {
        *guard = Some(stop_request_time);
    }
    // Reset counter BEFORE signaling stop so callbacks that fire after
    // STOP_REQUESTED is set are counted from a clean baseline.
    CALLBACKS_POST_STOP.store(0, Ordering::SeqCst);
    STOP_REQUESTED.store(true, Ordering::SeqCst);

    // Wait for the recording task to finish flushing (up to 2 seconds)
    let rx = {
        let mut rx_guard = STREAM_DONE_RX.lock().map_err(|e| e.to_string())?;
        rx_guard.take()
    };

    if let Some(rx) = rx {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), rx).await;
    }
    let wait_duration = stop_request_time.elapsed();
    let encoding_start_time = std::time::Instant::now();

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
        if duration_ms > 600_000 {
            return Err(format!(
                "Recording too long ({}ms, {:.1} minutes). Maximum is 10 minutes. Please split recordings.",
                duration_ms,
                duration_ms as f64 / 60_000.0
            ));
        }
    }

    let process_start = std::time::Instant::now();

    // Preserve native sample rate and use lossless FLAC encode for the online path.
    // This provides the API with maximum audio detail while FLAC compression
    // keeps the file size well within the 25MB limit for typical dictation.
    let flac_bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        let (processed, sample_rate) =
            process_audio_samples_online(samples, native_sample_rate as u32, native_channels);
        let _encode_start = std::time::Instant::now();
        let encoded = encode_flac_samples(&processed, sample_rate, 1);
        encoded
    })
    .await
    .map_err(|e| e.to_string())??;

    let process_duration = process_start.elapsed();

    let last_cb_time = TIMING_LAST_CALLBACK.lock().ok().and_then(|g| *g);
    let cb_delay = last_cb_time
        .map(|cb| {
            if cb > stop_request_time {
                cb.duration_since(stop_request_time).as_millis()
            } else {
                0
            }
        })
        .unwrap_or(0);

    log::info!(
        "Stop lifecycle diagnostics (flac): total wait = {:?}, last callback arrived {} ms after stop request, processing started {} ms after stop request",
        stop_request_time.elapsed(),
        cb_delay,
        encoding_start_time.duration_since(stop_request_time).as_millis()
    );

    log::info!(
        "stop_recording_flac_bytes performance: total = {:?}, wait stream = {:?}, process/encode = {:?}, rate = {}Hz, final size = {} bytes",
        stop_request_time.elapsed(),
        wait_duration,
        process_duration,
        native_sample_rate,
        flac_bytes.len()
    );

    Ok(flac_bytes)
}

pub async fn stop_recording_mp3_bytes() -> Result<Vec<u8>, String> {
    // Un-duck background apps on every exit path (errors, early returns) via RAII.
    let _restore_guard = crate::ducking::restore_guard();
    let _recording_owner_guard = crate::hotkey::RecordingStopGuard;
    // Give the microphone stream 100ms of continued recording after the hotkey release
    // before signalling stop. This matches v1.1.20's approach that eliminated truncation:
    // the final syllable lands in the buffer while the stream is still fully active.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let stop_request_time = std::time::Instant::now();
    if let Ok(mut guard) = TIMING_STOP_REQUESTED.lock() {
        *guard = Some(stop_request_time);
    }
    // Reset counter BEFORE signaling stop so callbacks that fire after
    // STOP_REQUESTED is set are counted from a clean baseline.
    CALLBACKS_POST_STOP.store(0, Ordering::SeqCst);
    STOP_REQUESTED.store(true, Ordering::SeqCst);

    // Wait for the recording task to finish flushing (up to 2 seconds)
    let rx = {
        let mut rx_guard = STREAM_DONE_RX.lock().map_err(|e| e.to_string())?;
        rx_guard.take()
    };

    if let Some(rx) = rx {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), rx).await;
    }
    let wait_duration = stop_request_time.elapsed();
    let encoding_start_time = std::time::Instant::now();

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
        if duration_ms > 600_000 {
            return Err(format!(
                "Recording too long ({}ms, {:.1} minutes). Maximum is 10 minutes. Please split recordings.",
                duration_ms,
                duration_ms as f64 / 60_000.0
            ));
        }
    }

    let process_start = std::time::Instant::now();

    // Run CPU-intensive audio processing on a dedicated thread to avoid blocking the async executor
    let mp3_bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        // Process audio using the validated v1.0.0 signal chain:
        // channel-averaged mono downmix, area-weighted box resampler to 16kHz,
        // DC offset removal, peak normalization to 0.9, 500ms silence padding.
        let (processed_samples, final_sample_rate) =
            process_audio_samples_online(samples, native_sample_rate as u32, native_channels);

        // Convert f32 samples to i16 for MP3 encoding
        let i16_samples: Vec<i16> = processed_samples
            .iter()
            .map(|&s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .collect();

        // Encode to MP3 using shine-rs (v1.0.0 behavior)
        let config = Mp3EncoderConfig::new()
            .sample_rate(final_sample_rate)
            .bitrate(64)
            .channels(1)
            .stereo_mode(StereoMode::Mono);

        let mut encoder = Mp3Encoder::new(config)
            .map_err(|e| format!("Failed to create MP3 encoder: {:?}", e))?;
        let samples_per_frame = encoder.samples_per_frame();
        let mut mp3_bytes = Vec::new();

        for chunk in i16_samples.chunks(samples_per_frame) {
            if chunk.len() == samples_per_frame {
                let frames = encoder
                    .encode_interleaved(chunk)
                    .map_err(|e| format!("MP3 encoding error: {:?}", e))?;
                for mut f in frames {
                    mp3_bytes.append(&mut f);
                }
            } else {
                let mut padded = chunk.to_vec();
                padded.resize(samples_per_frame, 0);
                let frames = encoder
                    .encode_interleaved(&padded)
                    .map_err(|e| format!("MP3 encoding error: {:?}", e))?;
                for mut f in frames {
                    mp3_bytes.append(&mut f);
                }
            }
        }

        let mut final_frames = encoder
            .finish()
            .map_err(|e| format!("Failed to finalize MP3 encoder: {:?}", e))?;
        mp3_bytes.append(&mut final_frames);

        Ok(mp3_bytes)
    })
    .await
    .map_err(|e| e.to_string())??;

    let process_duration = process_start.elapsed();

    let last_cb_time = TIMING_LAST_CALLBACK.lock().ok().and_then(|g| *g);
    let cb_delay = last_cb_time
        .map(|cb| {
            if cb > stop_request_time {
                cb.duration_since(stop_request_time).as_millis()
            } else {
                0
            }
        })
        .unwrap_or(0);

    log::info!(
        "Stop lifecycle diagnostics (mp3): total wait = {:?}, last callback arrived {} ms after stop request, processing started {} ms after stop request",
        stop_request_time.elapsed(),
        cb_delay,
        encoding_start_time.duration_since(stop_request_time).as_millis()
    );

    log::info!(
        "stop_recording_mp3_bytes performance: total = {:?}, wait stream = {:?}, process/encode = {:?}, rate = {}Hz, final size = {} bytes",
        stop_request_time.elapsed(),
        wait_duration,
        process_duration,
        native_sample_rate,
        mp3_bytes.len()
    );

    Ok(mp3_bytes)
}

#[allow(dead_code)]
pub async fn stop_recording_audio_bytes() -> Result<AudioPayload, String> {
    let settings = crate::settings::load_settings().map_err(|e| e.to_string())?;
    let preset = settings.stt_provider.preset.to_lowercase();

    if preset == "groq" || preset == "mistral" {
        let flac_bytes = stop_recording_flac_bytes().await?;
        Ok(AudioPayload {
            bytes: flac_bytes,
            mime_type: "audio/flac",
            filename: "audio.flac",
        })
    } else if preset == "openai" {
        let wav_bytes = stop_recording_wav_bytes().await?;
        Ok(AudioPayload {
            bytes: wav_bytes,
            mime_type: "audio/wav",
            filename: "audio.wav",
        })
    } else {
        let flac_bytes = stop_recording_flac_bytes().await?;
        Ok(AudioPayload {
            bytes: flac_bytes,
            mime_type: "audio/flac",
            filename: "audio.flac",
        })
    }
}

/// Stop recording and return the audio file as base64
#[tauri::command]
pub async fn stop_recording() -> Result<String, String> {
    let encode_start = std::time::Instant::now();
    let mp3_bytes = stop_recording_mp3_bytes().await?;
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &mp3_bytes);
    log::info!(
        "stop_recording base64 bridge: encode = {:?}, format = audio.mp3, size = {} bytes, b64 = {} bytes",
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

        assert_eq!(payload.mime_type, "audio/flac");
        assert_eq!(payload.filename, "audio.flac");
        assert!(payload.bytes.starts_with(b"fLaC"));
    }
}
