/**
 * Fluence Windows — UpdateManager
 * 
 * Professional 5-State Auto-Updater Service:
 * 1. idle       - "You're up to date" | Action: [Check for Updates]
 * 2. checking   - "Checking for updates..." | Action: [Checking...]
 * 3. available  - "Update available (vX.Y.Z)" | Action: [Download Update]
 * 4. downloading- "Downloading update (XX%)" | Progress Bar
 * 5. ready      - "Update ready to install" | Action: [Restart Fluence]
 * 6. failed     - "Couldn't check for updates" | Action: [Try Again]
 */

class UpdateManager {
  constructor() {
    this.state = 'idle'; // idle | checking | available | downloading | ready | failed
    this.updateObj = null;
    this.errorMessage = null;
    this.downloadProgress = 0;
    this.listeners = [];
    this.CHECK_INTERVAL_MS = 24 * 60 * 60 * 1000; // 24 hours
  }

  init() {
    // Wait 5 seconds after startup to perform background update check
    setTimeout(() => {
      this.checkWithPolicy();
    }, 5000);

    // Periodic check every hour to see if 24 hours have elapsed
    setInterval(() => {
      this.checkWithPolicy();
    }, 60 * 60 * 1000);
  }

  subscribe(callback) {
    this.listeners.push(callback);
    callback(this.getState());
    return () => {
      this.listeners = this.listeners.filter(cb => cb !== callback);
    };
  }

  notify() {
    const currentState = this.getState();
    this.listeners.forEach(cb => cb(currentState));
  }

  getLastCheckedText() {
    const ts = localStorage.getItem('fluence_last_update_check');
    if (!ts) return null;
    const diffMs = Date.now() - parseInt(ts, 10);
    if (diffMs < 60 * 1000) return 'Just now';
    if (diffMs < 60 * 60 * 1000) return `${Math.floor(diffMs / (60 * 1000))}m ago`;
    const date = new Date(parseInt(ts, 10));
    return `Today at ${date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}`;
  }

  getState() {
    return {
      state: this.state,
      updateObj: this.updateObj,
      downloadProgress: this.downloadProgress,
      errorMessage: this.errorMessage,
      lastCheckedText: this.getLastCheckedText(),
      version: this.updateObj ? this.updateObj.version : null,
      body: this.updateObj ? this.updateObj.body : null
    };
  }

  async checkWithPolicy() {
    if (this.state === 'downloading' || this.state === 'ready') return;

    const lastCheck = localStorage.getItem('fluence_last_update_check');
    const now = Date.now();
    if (lastCheck && (now - parseInt(lastCheck, 10)) < this.CHECK_INTERVAL_MS) {
      console.log('[UpdateManager] Skipping check — checked within last 24h');
      return;
    }

    if (await this.isUserRecording()) {
      console.log('[UpdateManager] Skipping check — user is currently recording');
      return;
    }

    await this.checkForUpdates(false);
  }

  async isUserRecording() {
    try {
      if (window.__TAURI__ && window.__TAURI__.core) {
        return await window.__TAURI__.core.invoke('is_recording');
      }
    } catch (e) {
      console.warn('[UpdateManager] Failed to check recording status:', e);
    }
    return false;
  }

  async checkForUpdates(manualTrigger = false) {
    // Guard against rapid duplicate clicks while check or download is in progress
    if (this.state === 'checking' || this.state === 'downloading') return;

    if (!window.__TAURI_PLUGIN_UPDATER__ && !window.__TAURI__?.updater) {
      console.log('[UpdateManager] Updater plugin not available in window context');
      if (manualTrigger) {
        this.state = 'failed';
        this.errorMessage = 'Updater plugin not initialized.';
        this.notify();
      }
      return;
    }

    this.state = 'checking';
    this.errorMessage = null;
    this.notify();

    try {
      if (await this.isUserRecording()) {
        console.log('[UpdateManager] Skipping check — user is currently recording');
        if (manualTrigger) {
          this.state = 'failed';
          this.errorMessage = 'Cannot check for updates while recording. Please finish recording first.';
          this.notify();
        }
        return;
      }

      const updater = window.__TAURI_PLUGIN_UPDATER__ || window.__TAURI__.updater;
      const update = await updater.check();

      localStorage.setItem('fluence_last_update_check', Date.now().toString());

      if (update && update.available) {
        this.updateObj = update;
        this.state = 'available';
        console.log(`[UpdateManager] Found update v${update.version}`);
      } else {
        this.updateObj = null;
        this.state = 'idle';
        console.log('[UpdateManager] Fluence is up to date.');
      }
    } catch (err) {
      console.error('[UpdateManager] Error checking for updates:', err);
      const errMsg = String(err?.message || err || '');
      const isNotFound = errMsg.includes('successful status code') || errMsg.includes('404') || errMsg.includes('Not Found');

      localStorage.setItem('fluence_last_update_check', Date.now().toString());

      if (manualTrigger) {
        // Manual user trigger: if 404, display precise feedback so deployment issues are never masked
        this.state = 'failed';
        this.errorMessage = isNotFound
          ? 'No release update feed published on GitHub yet (HTTP 404).'
          : 'Please check your internet connection or try again later.';
      } else {
        // Background silent check: return to idle quietly without popping up errors
        this.updateObj = null;
        this.state = 'idle';
        this.errorMessage = null;
        console.log('[UpdateManager] Background check: No release feed asset found on GitHub yet.');
      }
    }

    this.notify();
  }

  async startDownloadAndInstall() {
    if (!this.updateObj || this.state !== 'available') return;

    if (await this.isUserRecording()) {
      this.state = 'failed';
      this.errorMessage = 'Cannot update while recording. Please stop recording and try again.';
      this.notify();
      return;
    }

    this.state = 'downloading';
    this.downloadProgress = 0;
    this.errorMessage = null;
    this.notify();

    try {
      let downloadedBytes = 0;
      let totalBytes = 0;

      await this.updateObj.downloadAndInstall((event) => {
        switch (event.event) {
          case 'Started':
            totalBytes = event.data.contentLength || 0;
            console.log(`[UpdateManager] Download started. Size: ${totalBytes} bytes`);
            break;
          case 'Progress':
            downloadedBytes += event.data.chunkLength || 0;
            if (totalBytes > 0) {
              this.downloadProgress = Math.round((downloadedBytes / totalBytes) * 100);
            }
            this.notify();
            break;
          case 'Finished':
            console.log('[UpdateManager] Download & staging finished');
            break;
        }
      });

      this.state = 'ready';
      console.log('[UpdateManager] Update staged successfully. Ready to restart.');
    } catch (err) {
      console.error('[UpdateManager] Failed to download update:', err);
      this.state = 'failed';
      this.errorMessage = err.message || 'Failed to download update.';
    }

    this.notify();
  }

  async restartApp() {
    if (this.state !== 'ready') return;

    try {
      const processPlugin = window.__TAURI_PLUGIN_PROCESS__ || window.__TAURI__?.process;
      if (processPlugin && processPlugin.relaunch) {
        await processPlugin.relaunch();
      } else if (window.__TAURI__?.core) {
        await window.__TAURI__.core.invoke('plugin:process|restart');
      }
    } catch (err) {
      console.error('[UpdateManager] Failed to relaunch app:', err);
      this.state = 'failed';
      this.errorMessage = 'Failed to restart application automatically. Please restart manually.';
      this.notify();
    }
  }
}

// Global singleton instance
window.updateManager = new UpdateManager();
window.addEventListener('DOMContentLoaded', () => {
  window.updateManager.init();
});
