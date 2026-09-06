// Thin typed layer over the EXISTING Tauri IPC contract.
//
// Rules (backend freeze):
// - command names, argument shapes, and response shapes are reproduced
//   exactly as the vanilla frontend calls them — never renamed/reshaped.
// - when `window.__TAURI__` is absent (static preview), calls reject with a
//   clear error instead of throwing on property access.

interface TauriCore {
  invoke: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
}

interface TauriEventApi {
  listen: <T>(
    event: string,
    handler: (evt: { payload: T }) => void,
  ) => Promise<() => void>;
}

function core(): TauriCore {
  const c = (window as unknown as { __TAURI__?: { core?: TauriCore } })
    .__TAURI__?.core;
  if (!c) throw new Error('Tauri IPC unavailable in this window');
  return c;
}

function events(): TauriEventApi | null {
  return (
    (window as unknown as { __TAURI__?: { event?: TauriEventApi } }).__TAURI__
      ?.event ?? null
  );
}

export function invokeCmd<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  // Never throws synchronously: callers (including React effects) uniformly
  // handle absence of the Tauri runtime via promise rejection.
  try {
    return core().invoke<T>(cmd, args);
  } catch (err) {
    return Promise.reject(err);
  }
}

/** Returns a no-op unsubscriber when the event API is absent. */
export async function listenEvent<T>(
  event: string,
  handler: (payload: T) => void,
): Promise<() => void> {
  const api = events();
  if (!api) return () => undefined;
  return api.listen<T>(event, (evt) => handler(evt.payload));
}

// ——— Commands used by the shell + About slice (names/shapes = vanilla) ———

export const getAppVersion = () => invokeCmd<string>('get_app_version');

export const minimizeMainWindow = () =>
  invokeCmd<void>('minimize_main_window');

export const toggleMaximizeMainWindow = () =>
  invokeCmd<boolean>('toggle_maximize_main_window');

export const hideMainWindow = () => invokeCmd<void>('hide_main_window');

export const isRecording = () => invokeCmd<boolean>('is_recording');
