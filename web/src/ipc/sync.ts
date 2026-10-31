import { invokeCmd, listenEvent } from '@/ipc/tauri';

// Mirrors the `sync_get_status` payload shape (all fields optional — the
// backend may omit them; render logic defaults exactly like vanilla).
export interface SyncStatus {
  enabled?: boolean;
  signed_in?: boolean;
  account_key?: string | null;
  last_error?: string | null;
  running?: boolean;
  last_sync_at?: string | number | null;
  next_attempt_ms?: number | null;
  /** Automatic scheduling is paused after a fatal error; a user-triggered
   *  pass (Sync Now / re-enable) is required to resume. */
  waiting_for_retry?: boolean;
  /** F4 Success-with-warning: the latest pass converged with skipped remote
   *  state. Render as a warning alongside the (advanced) last_sync_at —
   *  never as an error and never as "not synced". */
  degraded?: boolean;
  /** Human-readable warning carried while `degraded` is set. */
  degraded_detail?: string | null;
}

// Command names + argument shapes reproduced exactly from vanilla.
export const getSyncStatus = () => invokeCmd<SyncStatus>('sync_get_status');

export const setSyncEnabled = (enabled: boolean) =>
  invokeCmd<SyncStatus>('sync_toggle', { enabled });

export const signInWithGoogle = () =>
  invokeCmd<SyncStatus>('sync_sign_in');

export const signOutGoogle = () => invokeCmd<void>('sync_sign_out');

/** Explicit "Sync Now": requests a pass and clears any fatal latch.
 *  Deliberately NOT `sync_toggle` — toggling is an enable/disable action and
 *  cannot re-arm a latched scheduler. */
export const runSyncNow = () => invokeCmd<SyncStatus>('sync_run_now');

export const subscribeSyncStatus = (handler: (status: SyncStatus) => void) =>
  listenEvent<SyncStatus>('sync-status', handler);
