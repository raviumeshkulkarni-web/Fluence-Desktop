import { useSyncExternalStore } from 'react';
import { invokeCmd, isRecording } from '@/ipc/tauri';

// Port of vanilla `UpdateManager` (src/js/update-manager.js): identical
// 6-state model, identical recording guard, identical 24h localStorage
// policy, identical user-facing strings. Differences from vanilla:
// - no DOM access: state is exposed via useSyncExternalStore.
// - background timers live in startBackgroundPolicy(), called once by App
//   (mirrors vanilla init()); manual paths behave exactly as before.
export type UpdaterState =
  | 'idle'
  | 'checking'
  | 'available'
  | 'downloading'
  | 'ready'
  | 'failed';

export interface UpdaterSnapshot {
  state: UpdaterState;
  version: string | null;
  body: string | null;
  progress: number;
  error: string | null;
  lastCheckedText: string | null;
}

interface UpdateObject {
  version: string;
  body?: string;
  downloadAndInstall: (
    onEvent: (event: { event: string; data?: Record<string, number> }) => void,
  ) => Promise<void>;
}

const LAST_CHECK_KEY = 'fluence_last_update_check';
const CHECK_INTERVAL_MS = 24 * 60 * 60 * 1000;

function lastCheckedText(): string | null {
  const ts = localStorage.getItem(LAST_CHECK_KEY);
  if (!ts) return null;
  const diff = Date.now() - parseInt(ts, 10);
  if (diff < 60 * 1000) return 'Just now';
  if (diff < 60 * 60 * 1000) return `${Math.floor(diff / (60 * 1000))}m ago`;
  const date = new Date(parseInt(ts, 10));
  return `Today at ${date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}`;
}

function updaterPlugin(): {
  check: () => Promise<UpdateObject | null>;
} | null {
  const w = window as unknown as Record<string, unknown>;
  return (
    (w.__TAURI_PLUGIN_UPDATER__ as { check: () => Promise<UpdateObject | null> }) ??
    (w.__TAURI__ as { updater?: { check: () => Promise<UpdateObject | null> } } | undefined)
      ?.updater ??
    null
  );
}

class UpdaterStore {
  private state: UpdaterState = 'idle';
  private updateObj: UpdateObject | null = null;
  private error: string | null = null;
  private progress = 0;
  private snapshot: UpdaterSnapshot = this.build();
  private listeners = new Set<() => void>();
  private policyTimer: number | null = null;

  subscribe = (cb: () => void) => {
    this.listeners.add(cb);
    return () => {
      this.listeners.delete(cb);
    };
  };

  getSnapshot = () => this.snapshot;

  private build(): UpdaterSnapshot {
    return {
      state: this.state,
      version: this.updateObj ? this.updateObj.version : null,
      body: this.updateObj?.body ?? null,
      progress: this.progress,
      error: this.error,
      lastCheckedText: lastCheckedText(),
    };
  }

  private emit() {
    this.snapshot = this.build();
    this.listeners.forEach((cb) => cb());
  }

  /** Mirrors vanilla init(): one delayed + one hourly policy evaluation. */
  startBackgroundPolicy() {
    if (this.policyTimer !== null) return;
    this.policyTimer = window.setTimeout(() => this.checkWithPolicy(), 5000);
    // Hourly evaluation like vanilla (process-lifetime timer, never cleared).
    window.setInterval(() => this.checkWithPolicy(), 60 * 60 * 1000);
  }

  private async checkWithPolicy() {
    if (this.state === 'downloading' || this.state === 'ready') return;
    const last = localStorage.getItem(LAST_CHECK_KEY);
    if (last && Date.now() - parseInt(last, 10) < CHECK_INTERVAL_MS) return;
    if (await this.recording()) return;
    await this.checkForUpdates(false);
  }

  private async recording(): Promise<boolean> {
    try {
      return await isRecording();
    } catch {
      return false;
    }
  }

  async checkForUpdates(manualTrigger = false) {
    if (this.state === 'checking' || this.state === 'downloading') return;
    const plugin = updaterPlugin();
    if (!plugin) {
      if (manualTrigger) {
        this.state = 'failed';
        this.error = 'Updater plugin not initialized.';
        this.emit();
      }
      return;
    }
    this.state = 'checking';
    this.error = null;
    this.emit();
    try {
      if (await this.recording()) {
        if (manualTrigger) {
          this.state = 'failed';
          this.error =
            'Cannot check for updates while recording. Please finish recording first.';
          this.emit();
        }
        return;
      }
      const update = await plugin.check();
      localStorage.setItem(LAST_CHECK_KEY, Date.now().toString());
      if (update) {
        this.updateObj = update;
        this.state = 'available';
      } else {
        this.updateObj = null;
        this.state = 'idle';
      }
    } catch (err) {
      const msg = String(
        (err as { message?: unknown } | null)?.message ?? err ?? '',
      );
      const notFound =
        msg.includes('successful status code') ||
        msg.includes('404') ||
        msg.includes('Not Found');
      localStorage.setItem(LAST_CHECK_KEY, Date.now().toString());
      if (manualTrigger) {
        this.state = 'failed';
        this.error = notFound
          ? 'No release update feed published on GitHub yet (HTTP 404).'
          : 'Please check your internet connection or try again later.';
      } else {
        this.updateObj = null;
        this.state = 'idle';
        this.error = null;
      }
    }
    this.emit();
  }

  async startDownloadAndInstall() {
    if (!this.updateObj || this.state !== 'available') return;
    if (await this.recording()) {
      this.state = 'failed';
      this.error =
        'Cannot update while recording. Please stop recording and try again.';
      this.emit();
      return;
    }
    this.state = 'downloading';
    this.progress = 0;
    this.error = null;
    this.emit();
    try {
      let downloaded = 0;
      let total = 0;
      await this.updateObj.downloadAndInstall((event) => {
        if (event.event === 'Started') {
          total = event.data?.contentLength ?? 0;
        } else if (event.event === 'Progress') {
          downloaded += event.data?.chunkLength ?? 0;
          if (total > 0) this.progress = Math.round((downloaded / total) * 100);
          this.emit();
        }
      });
      this.state = 'ready';
    } catch (err) {
      this.state = 'failed';
      this.error =
        (err as { message?: string } | null)?.message ??
        'Failed to download update.';
    }
    this.emit();
  }

  async restartApp() {
    if (this.state !== 'ready') return;
    try {
      const w = window as unknown as Record<string, unknown>;
      const processPlugin = (w.__TAURI_PLUGIN_PROCESS__ ??
        (w.__TAURI__ as { process?: { relaunch?: () => Promise<void> } })
          ?.process) as { relaunch?: () => Promise<void> } | undefined;
      if (processPlugin?.relaunch) {
        await processPlugin.relaunch();
      } else {
        await invokeCmd('plugin:process|restart');
      }
    } catch {
      this.state = 'failed';
      this.error =
        'Failed to restart application automatically. Please restart manually.';
      this.emit();
    }
  }
}

export const updaterStore = new UpdaterStore();

export function useUpdater(): UpdaterSnapshot {
  return useSyncExternalStore(
    updaterStore.subscribe,
    updaterStore.getSnapshot,
  );
}
