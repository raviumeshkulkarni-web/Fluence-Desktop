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
}

// Command names + argument shapes reproduced exactly from vanilla.
export const getSyncStatus = () => invokeCmd<SyncStatus>('sync_get_status');

export const setSyncEnabled = (enabled: boolean) =>
  invokeCmd<SyncStatus>('sync_toggle', { enabled });

export const signInWithGoogle = () =>
  invokeCmd<SyncStatus>('sync_sign_in');

export const signOutGoogle = () => invokeCmd<void>('sync_sign_out');

/** Vanilla "Sync Now" reuses sync_toggle with the current enabled value. */
export const triggerSyncNow = (enabled: boolean) =>
  invokeCmd<SyncStatus>('sync_toggle', { enabled });

export const subscribeSyncStatus = (handler: (status: SyncStatus) => void) =>
  listenEvent<SyncStatus>('sync-status', handler);
