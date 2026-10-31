import { useCallback, useEffect, useState } from 'react';
import { Button } from '@/components/ui/button';
import { Field } from '@/components/ui/field';
import { Switch } from '@/components/ui/switch';
import { toast } from '@/components/fluence/Toasts';
import { SettingsSectionHeader } from '@/components/fluence/SettingsSection';
import { describeSignInError } from '@/lib/signinError';
import {
  getSyncStatus,
  setSyncEnabled,
  signInWithGoogle,
  signOutGoogle,
  subscribeSyncStatus,
  runSyncNow,
  type SyncStatus,
} from '@/ipc/sync';

// Faithful port of the vanilla Sync surface (#page-sync + setupSyncPage,
// loadSyncPage, renderSyncStatus). Same DOM ids/classes, same copy, same
// toasts, same disabled logic. Two deliberate non-differences:
// - dictionary/snippets list refreshes after sign-in/out are skipped: those
//   routes are not migrated yet and own no React state to refresh.
// - the sync-status event is subscribed per-mount instead of globally;
//   visible behavior is identical.
// S7 audit note: the two toasts that still render `String(err)` (here and in
// `onToggle`) are deliberately left raw. Neither command can return sign-in or
// provider content — `sync_get_status` fails only on the ACL gate, and
// `sync_toggle` fails on that gate or on a local settings read/write — so
// routing them through `describeSignInError` would change user-visible copy
// without closing a provider-string leak. A local io error (a settings.json
// path) is still surfaced; that is a known residual, not an oversight.
export function SyncPage() {
  const [status, setStatus] = useState<SyncStatus | null>(null);
  const [pendingEnabled, setPendingEnabled] = useState<boolean | null>(null);
  const [signingIn, setSigningIn] = useState(false);

  const load = useCallback(async () => {
    try {
      setStatus(await getSyncStatus());
    } catch (err) {
      toast('Failed to load sync status: ' + String(err), 'error');
    }
  }, []);

  useEffect(() => {
    void load();
    // S7: guard against unmount-before-resolve — without `disposed`, an
    // unmount in this window leaks the subscription (cleanup sees undefined).
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void subscribeSyncStatus((s) => {
      if (!disposed) setStatus(s);
    }).then((u) => {
      if (disposed) {
        u();
      } else {
        unlisten = u;
      }
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [load]);

  const onToggle = async (enabled: boolean) => {
    setPendingEnabled(enabled);
    try {
      await setSyncEnabled(enabled);
      toast(
        enabled ? 'Background sync enabled' : 'Background sync disabled',
        'success',
      );
      await load();
    } catch (err) {
      toast(
        'Failed to update sync: ' + String(err).replace(/^Error:\s*/, ''),
        'error',
      );
    } finally {
      setPendingEnabled(null);
    }
  };

  const onSignIn = async () => {
    setSigningIn(true);
    try {
      const s = await signInWithGoogle();
      toast(
        'Signed in as ' + (s?.account_key || 'your Google account'),
        'success',
      );
      setStatus(s);
    } catch (err) {
      toast(describeSignInError(err), 'error');
      // The stored `last_error` is sanitized too, not just the toast: it is
      // component state read by `syncErr`, and keeping the raw provider string
      // around means the next render — or the next feature that surfaces
      // `last_error` verbatim — re-opens the same leak. `syncErr` maps every
      // sanitized string to the same fixed copy it produced from the raw one.
      setStatus((prev) => ({ ...(prev ?? {}), last_error: describeSignInError(err) }));
    } finally {
      setSigningIn(false);
    }
  };

  const onSignOut = async () => {
    try {
      await signOutGoogle();
      toast('Signed out. Existing sync data stays in Drive.', 'success');
      await load();
    } catch (err) {
      toast(describeSignInError(err, 'Sign-out'), 'error');
    }
  };

  const onSyncNow = async () => {
    if (!status?.enabled) {
      toast('Enable background sync first', 'error');
      return;
    }
    try {
      await runSyncNow();
      // The status snapshot returned by a command is taken before the
      // scheduler thread applies it, so it cannot prove the latch is cleared
      // yet; the follow-up sync-status event is the source of truth.
      toast('Sync requested', 'success');
    } catch (err) {
      toast(describeSignInError(err, 'Sync'), 'error');
    }
  };

  const s = status ?? {};
  const enabled = pendingEnabled ?? !!s.enabled;
  const signedIn = !!s.signed_in;
  const account = s.account_key || null;

  const accountBlock = signedIn && account
    ? {
        label: account,
        desc: 'Signed in. Your data syncs privately to your personal Drive folder.',
        signInVisible: false,
        signInText: 'Sign in with Google',
        signOutVisible: true,
      }
    : account
      ? {
          label: 'Reconnect Required',
          desc: `Google Drive access needs reauthorization. Reconnect as ${account} to resume syncing.`,
          signInVisible: true,
          signInText: 'Reconnect Google Drive',
          signOutVisible: false,
        }
      : {
          label: 'Not Signed In',
          desc: 'Sign in with Google to start syncing your data.',
          signInVisible: true,
          signInText: 'Sign in with Google',
          signOutVisible: false,
        };

  const statusText = !enabled
    ? 'Sync is off.'
    : s.running
      ? 'Syncing right now…'
      : s.waiting_for_retry
        ? 'Automatic sync paused after an error. Press Sync now to retry.'
        : s.last_sync_at
          ? `Last synced ${formatSyncStamp(s.last_sync_at)}${syncSuffix(s)}`
          : (syncErr(s.last_error) ?? 'Ready to sync.');

  const showSignInError = !signedIn && !!s.last_error;

  return (
    <section className="page active" id="page-sync">
      <div className="page-header">
        <h1 className="page-title" tabIndex={-1}>Sync</h1>
        <p className="page-subtitle">
          Back up your dictionary, snippets, usage stats, and preferences to your private Google Drive app storage. Transcripts and history never leave this device.
        </p>
      </div>

      <SettingsSectionHeader
        title="Cloud Sync"
        description="Configure automatic synchronization with your Google Drive"
      />
      <div className="settings-card">
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">Enable Background Sync</div>
            <div className="setting-desc">Automatically syncs every 15 minutes while signed in. Your data stays private to your account in your personal Drive folder.</div>
          </div>
          <div className="setting-control">
            <Field>
              <Switch
                id="sync-enabled-cb"
                aria-label="Enable background sync"
                checked={enabled}
                onCheckedChange={(v) => void onToggle(v)}
              />
            </Field>
          </div>
        </div>
      </div>

      <SettingsSectionHeader
        title="Account"
        description="Manage your connected cloud account and sync status"
      />
      <div className="settings-card">
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label" id="sync-account-label">{accountBlock.label}</div>
            <div className="setting-desc" id="sync-account-desc">{accountBlock.desc}</div>
            {showSignInError && (
              <div id="sync-signin-error" role="alert" className="field-error sync-signin-error">
                {syncErr(s.last_error)}
              </div>
            )}
          </div>
          <div className="setting-control">
            {accountBlock.signInVisible && (
              <Button
                variant="default"
                size="sm"
                id="sync-sign-in-btn"
                disabled={signingIn}
                onClick={() => void onSignIn()}
              >
                {signingIn ? 'Opening browser…' : accountBlock.signInText}
              </Button>
            )}
            {accountBlock.signOutVisible && (
              <Button variant="ghost" size="sm" id="sync-sign-out-btn" onClick={() => void onSignOut()}>
                Sign Out
              </Button>
            )}
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">Sync Status</div>
            <div className="setting-desc" id="sync-status-desc" role="status" aria-live="polite">{statusText}</div>
          </div>
          <div className="setting-control">
            <Button
              variant="ghost"
              size="sm"
              id="sync-now-btn"
              disabled={enabled ? !!s.running || !signedIn : !signedIn}
              onClick={() => void onSyncNow()}
            >
              Sync Now
            </Button>
          </div>
        </div>
      </div>
    </section>
  );
}

// Raw backend sync errors are never rendered; users get a friendly one-liner.
/** F4 Success-with-warning: a degraded pass converged, so the stamp stands
 *  and the suffix is a warning — never an error rendering. */
function syncSuffix(s: SyncStatus): string {
  if (s.degraded) return ' · Partial sync — some items were skipped';
  const err = syncErr(s.last_error);
  return err ? ` · ${err}` : '';
}

function syncErr(raw: unknown): string | null {
  if (!raw) return null;
  const m = String(raw).toLowerCase();
  if (/timeout|network|connection|dns/.test(m)) return 'Connection issue. Will retry automatically';
  if (/rate.?limit|quota|too many requests|\b429\b/.test(m)) return 'Google rate limit reached. Pausing briefly';
  if (/rejected|exceeds|too large/.test(m)) return 'Sync data exceeds size limits';
  if (/auth|credential|sign in again/.test(m)) return 'Google Drive access needs reauthorization';
  return 'Sync error. Will retry';
}

function formatSyncStamp(ts: string | number): string {
  const t = new Date(typeof ts === 'number' ? ts : ts);
  return t.toDateString() === new Date().toDateString()
    ? t.toLocaleTimeString()
    : t.toLocaleString();
}
