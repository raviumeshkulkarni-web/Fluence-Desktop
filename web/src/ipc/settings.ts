import { invokeCmd } from '@/ipc/tauri';

// Minimal settings-model port for routes migrated so far.
//
// Vanilla keeps one canonical `currentSettings` object, mutates it on every
// control change, and persists it debounced (350ms) via `update_settings`
// with the FULL object. This store reproduces exactly that for the fields
// migrated routes touch. The general auto-apply pipeline (hotkeys,
// autostart, providers batching, explicit Save flush) arrives with the
// General/Providers routes — until then only these fields are ever written,
// so behavior is identical.
export interface FluenceSettings {
  auto_learn_enabled?: boolean;
  auto_accept_enabled?: boolean;
  [key: string]: unknown;
}

let cached: FluenceSettings | null = null;
let persistTimer: number | null = null;
// Vanilla flush contract (flushPendingPersists): every queued write goes
// through one debounced timer, persists the FULL object, then runs
// per-feature side effects (hotkeys re-register, autostart apply).
// Featureless writes (dictionary/snippets/provider field writes) flush the
// object with no side effects — exactly as today.
let persistDirty = false;
const pendingFeatures = new Set<string>();
type PersistHook = () => Promise<void>;
const persistHooks = new Map<string, PersistHook>();
const listeners = new Set<() => void>();

function emit() {
  listeners.forEach((cb) => cb());
}

export function subscribeSettings(cb: () => void) {
  listeners.add(cb);
  return () => {
    listeners.delete(cb);
  };
}

export function getCachedSettings(): FluenceSettings | null {
  return cached;
}

export async function loadSettings(): Promise<FluenceSettings> {
  cached = await invokeCmd<FluenceSettings>('get_settings');
  emit();
  return cached;
}

function queuePersist(...features: string[]) {
  features.forEach((f) => pendingFeatures.add(f));
  persistDirty = true;
  if (persistTimer !== null) window.clearTimeout(persistTimer);
  persistTimer = window.setTimeout(() => {
    persistTimer = null;
    void flushSettings();
  }, 350);
}

async function runFlush(): Promise<void> {
  if (!cached || !persistDirty) return;
  persistDirty = false;
  const features = [...pendingFeatures];
  pendingFeatures.clear();
  let error: unknown = null;
  try {
    await invokeCmd('update_settings', { settings: cached });
  } catch (err) {
    error = err;
  }
  // Vanilla runs the feature hooks even when the settings write fails;
  // hooks surface their own failures exactly like vanilla.
  for (const f of features) {
    await persistHooks.get(f)?.();
  }
  if (error) throw error;
}

async function flushSettings() {
  try {
    await runFlush();
  } catch {
    // Vanilla shows a toast here; routes surface persistence failures
    // through their own UI. Kept silent to avoid duplicate toasts.
  }
}

/**
 * Register a post-persist side effect for one flush feature (vanilla
 * flushPendingPersists `hotkeys`/`autostart` branches). Runs once per
 * flush that queued the feature, after the settings write.
 */
export function registerPersistHook(feature: string, hook: PersistHook) {
  persistHooks.set(feature, hook);
}

/** Queue a debounced full-object persist, tagged with vanilla features. */
export function persist(...features: string[]) {
  queuePersist(...features);
}

/**
 * Immediate persist for explicit Save buttons (vanilla
 * flushPendingPersists + setupSaveButtons). Clears any debounced timer
 * first so the write happens exactly once. Rejects on failure so the
 * caller can toast, mirroring vanilla's error-then-success toast pair.
 */
export async function saveSettingsNow(): Promise<void> {
  if (persistTimer !== null) {
    window.clearTimeout(persistTimer);
    persistTimer = null;
  }
  await runFlush();
}

/** Mirrors vanilla auto-apply for a single field (debounced full persist). */
export function setSettingField(key: string, value: unknown) {
  // Fresh object: useSyncExternalStore bails out on identical snapshots,
  // so in-place mutation would never re-render subscribers.
  cached = { ...(cached ?? {}), [key]: value };
  emit();
  queuePersist();
}

/**
 * Mirrors vanilla syncLearnAcceptUI: turning auto-learn off forces
 * auto-accept off (persisted) and the dependent toggle is disabled with an
 * explanatory title until auto-learn is back on.
 */
export function setAutoLearn(on: boolean) {
  // Fresh object (see setSettingField): subscribers only re-render when
  // the snapshot reference changes.
  cached = { ...(cached ?? {}), auto_learn_enabled: on };
  if (!on) cached = { ...cached, auto_accept_enabled: false };
  emit();
  queuePersist();
}

export function isAutoLearn(s: FluenceSettings | null): boolean {
  return (s?.auto_learn_enabled ?? true) !== false;
}

export function isAutoAccept(s: FluenceSettings | null): boolean {
  return s?.auto_accept_enabled === true;
}
