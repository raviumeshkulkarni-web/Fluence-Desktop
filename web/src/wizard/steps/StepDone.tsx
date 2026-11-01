import { Check } from 'lucide-react';
import { Button } from '@/components/ui/button';
import type { WizardData } from '../Wizard';
import type { WizardSettings } from '../ipc';
import { invokeCmd } from '@/ipc/tauri';
import { closeWizard, showMainWindow, updateHotkeys, updateSettings } from '../ipc';
import { getLanguagePreference, t } from '@/i18n';
import { formatShortcut } from '@/lib/shortcut';

interface StepDoneProps {
  data: WizardData;
}

// Step 9: summary + finish actions. saveWizardSettings reproduces the
// vanilla settings blob and hotkey registration verbatim.
// The wizard blob is partial on purpose, but the clipboard opt-out must
// not be reset by a wizard re-run: read it before saving.
async function getStoredCopyTranscript(): Promise<{ copy_transcript_to_clipboard: boolean }> {
  try {
    const prev = await invokeCmd<{ copy_transcript_to_clipboard?: boolean }>('get_settings');
    return { copy_transcript_to_clipboard: prev.copy_transcript_to_clipboard ?? true };
  } catch {
    return { copy_transcript_to_clipboard: true };
  }
}

export function StepDone({ data }: StepDoneProps) {
  const save = async () => {
    try {
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
      const settings: WizardSettings = {
        hotkey: data.hotkey,
        recording_mode: data.recordingMode,
        overlay_position: data.overlayPosition,
        overlay_style: data.overlayStyle || 'full',
        stt_provider: {
          preset: usingOffline ? 'Local Offline' : data.provider,
          base_url: usingOffline ? '' : data.baseUrl,
          model: usingOffline ? 'sensevoice' : data.model,
          api_key_saved: !usingOffline,
        },
        llm_provider: {
          preset: data.provider === 'Local Offline' ? 'groq' : data.provider,
          base_url: data.provider === 'Local Offline' ? '' : data.baseUrl,
          model: data.provider === 'Local Offline' ? '' : data.llmModel,
          api_key_saved: !usingOffline,
        },
        auto_start: false,
        sound_on_complete: true,
        agent_mode_threshold_ms: 800,
        language: 'en',
        agent_hotkey: 'Ctrl+Shift+A',
        agent_recording_mode: 'push_to_toggle',
        ai_polish_style: 'none',
        auto_grab_highlight: true,
        audio_device_id: null,
        theme: resolvedTheme,
        ui_language: getLanguagePreference(),
        first_run: false,
        ...(await getStoredCopyTranscript()),
      };
      await updateSettings(settings);
      await updateHotkeys({
        transcriptionShortcut: data.hotkey,
        transcriptionMode: data.recordingMode,
        agentShortcut: 'Ctrl+Shift+A',
        agentMode: 'push_to_toggle',
      });
    } catch (err) {
      console.error('Failed to save wizard settings:', err);
    }
  };

  return (
    <>
      <div className="done-badge" aria-hidden="true">
        <Check size={40} strokeWidth={2.5} strokeLinecap="round" strokeLinejoin="round" />
      </div>
      <h1 className="step-title" tabIndex={-1}>
        {t('You\'re All Set!')}
      </h1>
      <p className="step-desc">
        {t('Fluence is running in your system tray. Press')}{' '}
        <span className="done-hotkey-chip">{formatShortcut(data.hotkey)}</span> {t('anywhere to start voice typing.')}
      </p>
      <div className="done-actions">
        <Button
          style={{ width: '100%', padding: 12 }}
          onClick={() =>
            void (async () => {
              await save();
              await closeWizard();
              await showMainWindow();
            })()
          }
        >
          {t('Open Settings')}
        </Button>
        <Button
          variant="ghost"
          style={{ width: '100%' }}
          onClick={() =>
            void (async () => {
              await save();
              await closeWizard();
            })()
          }
        >
          {t('Close Wizard')}
        </Button>
      </div>
    </>
  );
}
