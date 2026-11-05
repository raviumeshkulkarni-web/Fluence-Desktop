import { useState } from 'react';
import { Check } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { describeError } from '@/lib/errors';
import type { WizardData } from '../Wizard';
import type { AppSettings } from '../ipc';
import { closeWizard, getSettings, showMainWindow, updateHotkeys, updateSettings } from '../ipc';
import { displayHotkey } from '../hotkey';

interface StepDoneProps {
  data: WizardData;
}

class SetupError extends Error {}

// Step 9: summary + finish actions. The stored settings are read first and
// only the fields the wizard owns are overridden — the backend replaces the
// whole object, so a partial payload would reset unrelated preferences
// (waveform, ducking, sync, cleaner provider, clipboard opt-out, ...).
export function StepDone({ data }: StepDoneProps) {
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const persist = async () => {
    const current = await getSettings();
    const usingOffline = !!data.skipApiKey || data.provider === 'Local Offline';
    // The settings shell follows the stored choice live — 'auto' tracks
    // the OS with no restart. The backend blob only speaks dark|light, so
    // it records the currently-resolved value for consistency.
    const storedTheme: string =
      data.themeMode === 'light' || data.themeMode === 'auto' ? data.themeMode : 'dark';
    const resolvedTheme =
      storedTheme === 'light' ||
      (storedTheme === 'auto' &&
        window.matchMedia?.('(prefers-color-scheme: light)').matches)
        ? 'light'
        : 'dark';
    try {
      window.localStorage.setItem('fluence_theme', storedTheme);
    } catch {
      // Backend blob below still carries the resolved choice.
    }

    const next: AppSettings = {
      ...current,
      hotkey: data.hotkey,
      recording_mode: data.recordingMode,
      overlay_position: data.overlayPosition,
      overlay_style: data.overlayStyle || 'full',
      theme: resolvedTheme,
      first_run: false,
    };

    if (usingOffline) {
      next.stt_provider = {
        preset: 'Local Offline',
        base_url: '',
        model: 'sensevoice',
        api_key_saved: false,
      };
    } else {
      next.stt_provider = {
        preset: data.provider,
        base_url: data.baseUrl,
        model: data.model,
        api_key_saved: true,
      };
      // The key is opt-in for the LLM side: leaving this unchecked keeps the
      // user's existing AI cleanup / Agent Mode provider untouched.
      if (data.useKeyForAgent) {
        next.llm_provider = {
          preset: data.provider,
          base_url: data.baseUrl,
          model: data.llmModel,
          api_key_saved: true,
        };
      }
    }

    const previousHotkeys = {
      transcriptionShortcut: current.hotkey,
      transcriptionMode: current.recording_mode,
      agentShortcut: current.agent_hotkey,
      agentMode: current.agent_recording_mode,
    };

    // Register first (registration does not persist settings). A conflict or
    // invalid shortcut must abort before first_run is cleared.
    try {
      await updateHotkeys({
        transcriptionShortcut: data.hotkey,
        transcriptionMode: data.recordingMode,
        agentShortcut: current.agent_hotkey,
        agentMode: current.agent_recording_mode,
      });
    } catch (err) {
      // register_hotkeys unregisters the current pair before registering the
      // new one, so a conflict or invalid agent shortcut can leave both dead.
      // Restore the saved pair before surfacing the failure.
      await updateHotkeys(previousHotkeys).catch(() => undefined);
      throw new SetupError(
        `The shortcut "${displayHotkey(data.hotkey)}" could not be registered. ${describeError(err)}`,
      );
    }

    try {
      await updateSettings(next);
    } catch (err) {
      // Roll the global shortcut back to the last saved one: settings were not
      // written, so leaving the new shortcut active would only last until quit.
      await updateHotkeys(previousHotkeys).catch(() => undefined);
      throw new SetupError(`Your setup could not be saved. ${describeError(err)}`);
    }
  };

  const finish = async (openSettings: boolean) => {
    if (saving) return;
    setSaving(true);
    setError(null);
    try {
      await persist();
    } catch (err) {
      setError(
        err instanceof SetupError
          ? err.message
          : `Setup could not be completed. ${describeError(err)}`,
      );
      setSaving(false);
      return;
    }
    try {
      // Open the destination first so a failure is still visible in the
      // wizard; only hide the wizard once there is somewhere to land.
      if (openSettings) await showMainWindow();
      await closeWizard();
    } catch (err) {
      setError(`Your setup was saved, but Fluence could not finish opening. ${describeError(err)}`);
    } finally {
      setSaving(false);
    }
  };

  return (
    <>
      <div className="done-badge" aria-hidden="true">
        <Check size={40} strokeWidth={2.5} strokeLinecap="round" strokeLinejoin="round" />
      </div>
      <h1 className="step-title" tabIndex={-1}>
        Finish Setup
      </h1>
      <p className="step-desc">
        Your choices are ready. Finishing saves them and keeps{' '}
        <span className="done-hotkey-chip">{displayHotkey(data.hotkey)}</span> available anywhere.
      </p>
      {error && (
        <div className="wizard-save-error" role="alert">
          {error}
        </div>
      )}
      <div className="done-actions">
        <Button
          style={{ width: '100%', padding: 12 }}
          disabled={saving}
          onClick={() => void finish(true)}
        >
          {saving ? 'Saving…' : 'Save and Open Settings'}
        </Button>
        <Button
          variant="ghost"
          style={{ width: '100%' }}
          disabled={saving}
          onClick={() => void finish(false)}
        >
          Save and Close
        </Button>
      </div>
    </>
  );
}
