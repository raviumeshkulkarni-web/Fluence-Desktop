import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

import {
  getOfflineModelStatus,
  downloadOfflineModel,
  cancelOfflineDownload,
  getMoonshineV2SmallModelStatus,
  downloadMoonshineV2SmallModel,
  subscribeOfflineDownloadProgress,
  stopAndTranscribeRecording,
} from './ipc.ts';

const DIR = dirname(fileURLToPath(import.meta.url));

type Call = { cmd: string; args?: Record<string, unknown> };
const calls: Call[] = [];
const eventListeners: { event: string; handler: (evt: { payload: unknown }) => void }[] = [];

(globalThis as Record<string, unknown>).window = {
  __TAURI__: {
    core: {
      invoke: (cmd: string, args?: Record<string, unknown>) => {
        calls.push({ cmd, args });
        if (cmd === 'get_offline_model_status' || cmd === 'get_moonshine_v2_small_model_status') {
          return Promise.resolve(true);
        }
        if (cmd === 'stop_and_transcribe_recording') {
          return Promise.resolve({
            text: 'Hello world',
            rawText: 'Hello world',
            durationMs: 120,
            provider: 'Local Offline',
          });
        }
        return Promise.resolve();
      },
    },
    event: {
      listen: (event: string, handler: (evt: { payload: unknown }) => void) => {
        eventListeners.push({ event, handler });
        return Promise.resolve(() => undefined);
      },
    },
  },
};

test('wizard is configured for exactly 5 steps in Wizard.tsx', () => {
  const wizardSrc = readFileSync(join(DIR, 'Wizard.tsx'), 'utf-8');
  assert.match(wizardSrc, /export const TOTAL_STEPS = 5;/);
  assert.match(wizardSrc, /offlineEngine:\s*'moonshine_v2_small'/);
  assert.match(wizardSrc, /offlineInstalled:\s*false/);
  assert.match(wizardSrc, /offlineDownloadSkipped:\s*false/);
});

test('offline model status and download wrappers invoke expected Tauri commands', async () => {
  calls.length = 0;

  const status1 = await getOfflineModelStatus();
  assert.equal(status1, true);
  assert.equal(calls[calls.length - 1]?.cmd, 'get_offline_model_status');

  await downloadOfflineModel();
  assert.equal(calls[calls.length - 1]?.cmd, 'download_offline_model');

  await cancelOfflineDownload();
  assert.equal(calls[calls.length - 1]?.cmd, 'cancel_offline_download');

  const status2 = await getMoonshineV2SmallModelStatus();
  assert.equal(status2, true);
  assert.equal(calls[calls.length - 1]?.cmd, 'get_moonshine_v2_small_model_status');

  await downloadMoonshineV2SmallModel();
  assert.equal(calls[calls.length - 1]?.cmd, 'download_moonshine_v2_small_model');

  const res = await stopAndTranscribeRecording();
  assert.equal(res.text, 'Hello world');
  assert.equal(calls[calls.length - 1]?.cmd, 'stop_and_transcribe_recording');
});

test('subscribeOfflineDownloadProgress listens to offline-download-progress event', async () => {
  eventListeners.length = 0;
  let receivedPayload: unknown = null;

  await subscribeOfflineDownloadProgress((p) => {
    receivedPayload = p;
  });

  assert.equal(eventListeners.length, 1);
  assert.equal(eventListeners[0]?.event, 'offline-download-progress');

  // Trigger event listener
  eventListeners[0]?.handler({
    payload: {
      status: 'downloading',
      progress: 45.5,
      bytesDownloaded: 45500,
      totalBytes: 100000,
      currentFile: 'model.onnx',
    },
  });

  assert.deepEqual(receivedPayload, {
    status: 'downloading',
    progress: 45.5,
    bytesDownloaded: 45500,
    totalBytes: 100000,
    currentFile: 'model.onnx',
  });
});

test('wizard polish fixes are applied', () => {
  const wizardSrc = readFileSync(join(DIR, 'Wizard.tsx'), 'utf-8');
  assert.match(wizardSrc, /cancelOfflineDownload/);
  assert.match(wizardSrc, /void\s+cancelOfflineDownload\(\)\.catch\(\(\)\s*=>\s*undefined\);\s*void\s+closeWizard\(\);/);

  const apiKeySrc = readFileSync(join(DIR, 'steps', 'StepApiKey.tsx'), 'utf-8');
  assert.match(apiKeySrc, /Fast \(English, Recommended\)/);
  assert.doesNotMatch(apiKeySrc, /Fast — English/);
  assert.match(apiKeySrc, /key\s*&&\s*!cancelled\s*&&\s*!dataRef\.current\.apiKey/);
  // Ensure offline error banner is rendered outside the ternary
  const errorBannerIdx = apiKeySrc.indexOf('{(error || dlError) &&');
  const ternaryIdx = apiKeySrc.indexOf('{data.offlineInstalled ?');
  assert.ok(errorBannerIdx !== -1, 'error banner exists in StepApiKey');
  assert.ok(ternaryIdx !== -1, 'ternary exists in StepApiKey');
  assert.ok(errorBannerIdx < ternaryIdx, 'error banner rendered outside (before) ternary');

  const testSrc = readFileSync(join(DIR, 'steps', 'StepTest.tsx'), 'utf-8');
  assert.match(testSrc, /No speech detected: try speaking a little closer to your mic\./);
  assert.doesNotMatch(testSrc, /No speech detected —/);
  assert.match(testSrc, /let\s+previousSettings:\s*AppSettings\s*\|\s*null\s*=\s*null;/);
  assert.match(testSrc, /previousSettingsRef\.current\s*=\s*previousSettings;/);

  // Check offline_downloader.rs cleanup before drop(guard)
  const downloaderSrc = readFileSync(join(DIR, '..', '..', '..', 'src-tauri', 'src', 'offline_downloader.rs'), 'utf-8');
  assert.match(
    downloaderSrc,
    /clean_temp_files\(&get_offline_dir\(\)\);[\s\S]*?drop\(guard\);[\s\S]*?emit_progress/
  );
  assert.match(
    downloaderSrc,
    /for\s+f\s+in\s+&manifest\.files[\s\S]*?clean_temp_files\(dest_dir\);[\s\S]*?drop\(guard\);[\s\S]*?emit_progress/
  );
});
