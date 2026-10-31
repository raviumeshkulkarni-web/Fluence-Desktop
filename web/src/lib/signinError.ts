// Sign-in / sign-out failure copy for the LIVE web tree.
//
// Audit finding S7 (live-surface gap): the sanitizer existed only as
// `describeSignInError` in the legacy vanilla `src/js/settings.js`, which is
// NOT loaded by the shipping app — `tauri.conf.json` points `frontendDist` at
// `../src` and serves `src/dist/index.html` (the Vite bundle). The React page
// concatenated the raw backend error into a user-visible toast, so
// `AuthError::Network` rendered Google's token endpoint URL and
// `AuthError::Http` rendered the raw provider JSON body, while the regression
// guard in tests/signin-toast-sanitization.test.mjs stayed green because it only
// exercised the dead vanilla copy. This module is the LIVE equivalent; the
// guard now covers both.
//
// Deliberately PURE (no IPC, no React, no DOM), mirroring `lib/claim.ts`: the
// mapping rules stay unit-testable and cannot drift between the two surfaces.
//
// One message MUST survive verbatim: the no-refresh-token error. It is
// user-facing copy by design (it tells the user to revoke access at
// myaccount.google.com/permissions and retry), so allowlisting it comes FIRST,
// before any URL stripping — otherwise its own remedy URL would be cut.
// Everything else renders as fixed copy; nothing raw ever reaches the toast.
//
// `action` prefixes the generic fallbacks so the same mapper serves every
// failing button on the Sync page without string surgery at the call site.
// 'Sync' exists because SyncPage also renders the raw `sync_run_now` failure:
// routing that toast through this mapper keeps the same no-raw-string
// guarantee while still saying the truthful thing ("Sync failed", not
// "Sign-in failed", for a button that was never about signing in).
import { t } from '@/i18n';

/** Which user action produced the failure; only affects the fallback prefixes. */
export type FailedAction = 'Sign-in' | 'Sign-out' | 'Sync';

/**
 * Maps a raw sign-in/sign-out/sync backend error onto text that is safe to
 * render.
 *
 * Returns a complete toast body (no prefix/suffix is added by the caller).
 * A returned string never contains a provider URL, provider response body,
 * markup, or a long diagnostic dump.
 */
export function describeSignInError(
  raw: unknown,
  action?: FailedAction,
): string {
  const label: string = action ? t(action) : t('Sign-in');
  const text = String(raw ?? '').replace(/^Error:\s*/, '');
  // The actionable no-refresh-token message is user-facing by design.
  // Matched on its stable prefix so future copy edits that keep the prefix
  // keep the allowlist; the myaccount URL inside it is intentional.
  if (/did not return a refresh token/i.test(text)) return text;
  const lower = text.toLowerCase();
  // Browser could not be opened: an OS condition, never a provider string.
  if (/failed to open the browser/i.test(text))
    return t('Could not open the browser for sign-in. Please open it manually and try again.');
  // Identity lookup and token-exchange failures carry endpoint URLs and
  // provider bodies. Map them; never render them.
  if (/account lookup failed/i.test(text))
    return t('Could not confirm the Google account. Check your connection and try again.');
  if (/token endpoint|token exchange|authorization code/i.test(text))
    return t('Google sign-in did not complete. Please try again.');
  if (/timeout|network|connection|dns|offline/i.test(lower))
    return t('Connection issue. Check your connection and try signing in again.');
  if (/access denied|forbidden|401|403/i.test(text))
    return t('Google refused the sign-in request. Please try again.');
  // Anything with a URL or markup is a provider string by construction.
  if (/https?:\/\/|<[^>]+>/.test(text))
    return t('{action} failed. Please try again.', { action: label });
  // Unknown but short plain text without URLs is safe to show; anything long
  // is a diagnostic dump, not a message.
  if (text.length > 0 && text.length <= 160 && !/[{}\[\]"]/.test(text))
    return t('{action} failed: {error}', { action: label, error: text });
  return t('{action} failed. Please try again.', { action: label });
}
