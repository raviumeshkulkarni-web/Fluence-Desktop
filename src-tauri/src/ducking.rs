// Fluence Windows — Audio ducking module
// Mutes/attenuates other apps' render sessions while dictating (Core Audio),
// restoring each session's exact prior (volume, mute). State is plain data only —
// no COM pointers cross calls — so every duck/restore re-enumerates live sessions
// and matches by saved identifiers. A sidecar JSON makes a crash recoverable.

/// RAII guard: restores ducked sessions when dropped. Placed at the top of each
/// shared capture-stop function so restore fires on every exit path (errors, early
/// returns, panics), not only the happy path. No-op when nothing is ducked.
pub struct RestoreGuard;

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        restore();
    }
}

pub fn restore_guard() -> RestoreGuard {
    RestoreGuard
}

#[cfg(target_os = "windows")]
pub use sys::{duck, replay_on_launch, restore};

#[cfg(not(target_os = "windows"))]
pub fn duck(_level: f32) {}
#[cfg(not(target_os = "windows"))]
pub fn restore() {}
#[cfg(not(target_os = "windows"))]
pub fn replay_on_launch() {}

#[cfg(target_os = "windows")]
mod sys {
    use core::ffi::c_void;
    use once_cell::sync::Lazy;
    use serde::{Deserialize, Serialize};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use windows::core::Interface;
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::Media::Audio::{
        eConsole, eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
        ISimpleAudioVolume, MMDeviceEnumerator,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        COINIT_MULTITHREADED,
    };

    /// One saved render session: identity keys + the original (volume, mute) to restore.
    #[derive(Clone, Serialize, Deserialize)]
    struct DuckEntry {
        endpoint_id: String,
        session_identifier: String,
        session_instance_identifier: String,
        pid: u32,
        volume: f32,
        mute: bool,
    }

    /// `active_duck`: sessions ducked in the current cycle (restored on stop).
    /// `pending_recovery`: entries whose session vanished (app closed while ducked) —
    /// its registry-persisted volume stays ducked, so we retain and reconcile later.
    #[derive(Default, Serialize, Deserialize)]
    struct DuckState {
        active_duck: Vec<DuckEntry>,
        pending_recovery: Vec<DuckEntry>,
    }

    // Single lock serializing every transition (duck/restore/reconcile/replay/exit)
    // so timer, command, and exit paths can't race on the state or the sidecar.
    static STATE: Lazy<Mutex<DuckState>> = Lazy::new(|| Mutex::new(DuckState::default()));

    // Ensures at most one background reconciliation task is alive at a time.
    static RECONCILER_ACTIVE: AtomicBool = AtomicBool::new(false);

    /// A live session discovered during enumeration, holding its COM volume interface.
    /// Lives only on the COM worker thread — never stored in `DuckState`.
    struct LiveSession {
        endpoint_id: String,
        session_identifier: String,
        session_instance_identifier: String,
        pid: u32,
        volume: ISimpleAudioVolume,
    }

    fn sidecar_path() -> PathBuf {
        let mut path = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
        path.push("Fluence");
        path.push("duck-restore.json");
        path
    }

    /// Run `f` on a dedicated thread with its own COM apartment, then join. Isolates
    /// COM from Tauri's tokio runtime (whose worker/main threads have their own
    /// apartment state) and lets every entry point call ducking synchronously.
    fn run_com<F: FnOnce() + Send + 'static>(f: F) {
        let handle = std::thread::spawn(move || unsafe {
            let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
            f();
            if hr.is_ok() {
                CoUninitialize();
            }
        });
        if handle.join().is_err() {
            log::warn!("ducking COM worker thread panicked");
        }
    }

    /// Read a COM-allocated wide string and free it with CoTaskMemFree (caller owns it).
    unsafe fn take_pwstr(p: windows::core::PWSTR) -> String {
        if p.is_null() {
            return String::new();
        }
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s
    }

    /// Enumerate render sessions on the default output endpoint, skipping our own PID.
    unsafe fn enumerate_sessions() -> windows::core::Result<Vec<LiveSession>> {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let endpoint_id = take_pwstr(device.GetId()?);
        let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
        let session_enum = manager.GetSessionEnumerator()?;
        let count = session_enum.GetCount()?;
        let own_pid = std::process::id();

        let mut out = Vec::new();
        for i in 0..count {
            let ctrl = match session_enum.GetSession(i) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let ctrl2: IAudioSessionControl2 = match ctrl.cast() {
                Ok(c) => c,
                Err(_) => continue,
            };
            let pid = ctrl2.GetProcessId().unwrap_or(0);
            if pid == own_pid {
                continue;
            }
            let volume: ISimpleAudioVolume = match ctrl2.cast() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let session_identifier = ctrl2
                .GetSessionIdentifier()
                .map(|p| take_pwstr(p))
                .unwrap_or_default();
            let session_instance_identifier = ctrl2
                .GetSessionInstanceIdentifier()
                .map(|p| take_pwstr(p))
                .unwrap_or_default();
            out.push(LiveSession {
                endpoint_id: endpoint_id.clone(),
                session_identifier,
                session_instance_identifier,
                pid,
                volume,
            });
        }
        Ok(out)
    }

    /// Apply the duck: full mute when level is 0.0, else scale current volume by level
    /// (multiplicative — preserves the user's relative mix).
    unsafe fn apply_duck(vol: &ISimpleAudioVolume, level: f32, current: f32) {
        if level <= 0.0 {
            let _ = vol.SetMute(BOOL(1), std::ptr::null());
        } else {
            let _ = vol.SetMasterVolume(current * level, std::ptr::null());
        }
    }

    unsafe fn apply_restore(vol: &ISimpleAudioVolume, volume: f32, mute: bool) {
        let _ = vol.SetMasterVolume(volume, std::ptr::null());
        let _ = vol.SetMute(BOOL(mute as i32), std::ptr::null());
    }

    /// Restore any `pending_recovery` entry whose session has reappeared, but only on an
    /// unambiguous 1:1 match by session_identifier (exactly one pending entry and exactly
    /// one live session share it) — otherwise we might restore the wrong app instance.
    unsafe fn reconcile_pending(st: &mut DuckState, live: &[LiveSession]) {
        if st.pending_recovery.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut st.pending_recovery);
        let mut still = Vec::new();
        for entry in pending.iter() {
            let pending_dupes = pending
                .iter()
                .filter(|e| {
                    e.endpoint_id == entry.endpoint_id
                        && e.session_identifier == entry.session_identifier
                })
                .count();
            let live_matches: Vec<&LiveSession> = live
                .iter()
                .filter(|l| {
                    l.endpoint_id == entry.endpoint_id
                        && l.session_identifier == entry.session_identifier
                })
                .collect();
            if pending_dupes == 1 && live_matches.len() == 1 {
                apply_restore(&live_matches[0].volume, entry.volume, entry.mute);
                log::info!(
                    "ducking: reconciled reopened session {}",
                    entry.session_identifier
                );
            } else {
                still.push(entry.clone());
            }
        }
        st.pending_recovery = still;
    }

    /// Persist the union of both states atomically (temp + rename); delete the sidecar
    /// only when both are empty. Caller holds the STATE lock.
    fn persist(st: &DuckState) {
        let path = sidecar_path();
        if st.active_duck.is_empty() && st.pending_recovery.is_empty() {
            let _ = std::fs::remove_file(&path);
            return;
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string(st) {
            Ok(data) => {
                let tmp = path.with_extension("json.tmp");
                if std::fs::write(&tmp, data).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                } else {
                    log::warn!("ducking: failed to write sidecar temp file");
                }
            }
            Err(e) => log::warn!("ducking: sidecar serialize failed: {e}"),
        }
    }

    pub fn duck(level: f32) {
        run_com(move || unsafe {
            let live = match enumerate_sessions() {
                Ok(l) => l,
                Err(e) => {
                    log::warn!("ducking: duck enumeration failed: {e}");
                    return;
                }
            };
            let mut st = STATE.lock().unwrap();
            // Idempotent within the active cycle: a second duck (no intervening restore)
            // must not re-snapshot already-ducked levels as "original".
            if !st.active_duck.is_empty() {
                return;
            }
            // Resolve reopened sessions to their true original BEFORE snapshotting, so a
            // still-ducked level is never captured as the new "original".
            reconcile_pending(&mut st, &live);
            for ls in &live {
                let current = ls.volume.GetMasterVolume().unwrap_or(1.0);
                let mute = ls.volume.GetMute().map(|b| b.as_bool()).unwrap_or(false);
                apply_duck(&ls.volume, level, current);
                st.active_duck.push(DuckEntry {
                    endpoint_id: ls.endpoint_id.clone(),
                    session_identifier: ls.session_identifier.clone(),
                    session_instance_identifier: ls.session_instance_identifier.clone(),
                    pid: ls.pid,
                    volume: current,
                    mute,
                });
            }
            persist(&st);
            log::info!(
                "ducking: ducked {} session(s) at level {level}",
                st.active_duck.len()
            );
        });
        kick_reconciler_if_pending();
    }

    pub fn restore() {
        // Cheap short-circuit so the disabled path (guard fires on every stop) never
        // spawns a COM thread when there is nothing to restore or reconcile.
        {
            let st = STATE.lock().unwrap();
            if st.active_duck.is_empty() && st.pending_recovery.is_empty() {
                return;
            }
        }
        run_com(|| unsafe {
            let live = match enumerate_sessions() {
                Ok(l) => l,
                Err(e) => {
                    log::warn!("ducking: restore enumeration failed: {e}");
                    return;
                }
            };
            let mut st = STATE.lock().unwrap();
            // Restore active entries by session_instance_identifier (unique per app
            // instance); entries whose session vanished move to pending_recovery.
            let active = std::mem::take(&mut st.active_duck);
            for entry in active {
                match live.iter().find(|l| {
                    l.endpoint_id == entry.endpoint_id
                        && l.session_instance_identifier == entry.session_instance_identifier
                }) {
                    Some(ls) => apply_restore(&ls.volume, entry.volume, entry.mute),
                    None => st.pending_recovery.push(entry),
                }
            }
            reconcile_pending(&mut st, &live);
            persist(&st);
        });
        kick_reconciler_if_pending();
    }

    /// Reconcile pending entries against live sessions once. Returns true once drained.
    fn reconcile_once() -> bool {
        {
            let st = STATE.lock().unwrap();
            if st.pending_recovery.is_empty() {
                return true;
            }
        }
        run_com(|| unsafe {
            let live = match enumerate_sessions() {
                Ok(l) => l,
                Err(e) => {
                    log::warn!("ducking: reconcile enumeration failed: {e}");
                    return;
                }
            };
            let mut st = STATE.lock().unwrap();
            reconcile_pending(&mut st, &live);
            persist(&st);
        });
        STATE.lock().unwrap().pending_recovery.is_empty()
    }

    /// Start the background reconciliation tick if not already running. It polls every
    /// ~30 s while entries remain unresolved (an app closed while ducked, awaiting its
    /// reopen) and exits once they drain — so no timer runs when there's nothing to do.
    fn ensure_reconciler() {
        if RECONCILER_ACTIVE.swap(true, Ordering::SeqCst) {
            return;
        }
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                if reconcile_once() {
                    break;
                }
            }
            RECONCILER_ACTIVE.store(false, Ordering::SeqCst);
            // Close the race where an entry is added just as we exit: re-arm if needed.
            if !STATE.lock().unwrap().pending_recovery.is_empty() {
                ensure_reconciler();
            }
        });
    }

    fn kick_reconciler_if_pending() {
        if !STATE.lock().unwrap().pending_recovery.is_empty() {
            ensure_reconciler();
        }
    }

    /// Replay a sidecar left by a crashed session: load its entries as unresolved
    /// (post-crash, instance ids are stale, so match crash-safe by session_identifier),
    /// reconcile once against live sessions, and keep the tick alive for any still absent.
    pub fn replay_on_launch() {
        {
            let path = sidecar_path();
            let data = match std::fs::read_to_string(&path) {
                Ok(d) => d,
                Err(_) => return,
            };
            let saved: DuckState = match serde_json::from_str(&data) {
                Ok(s) => s,
                Err(e) => {
                    log::warn!("ducking: sidecar parse failed, discarding: {e}");
                    let _ = std::fs::remove_file(&path);
                    return;
                }
            };
            let mut st = STATE.lock().unwrap();
            st.pending_recovery = saved
                .active_duck
                .into_iter()
                .chain(saved.pending_recovery)
                .collect();
            st.active_duck.clear();
            log::info!(
                "ducking: replaying {} entr(ies) from crash sidecar",
                st.pending_recovery.len()
            );
        }
        run_com(|| unsafe {
            let live = match enumerate_sessions() {
                Ok(l) => l,
                Err(e) => {
                    log::warn!("ducking: launch reconcile enumeration failed: {e}");
                    return;
                }
            };
            let mut st = STATE.lock().unwrap();
            reconcile_pending(&mut st, &live);
            persist(&st);
        });
        kick_reconciler_if_pending();
    }
}
