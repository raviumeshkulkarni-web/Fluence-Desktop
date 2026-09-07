import { Check } from 'lucide-react';
import { Button } from '@/components/ui/button';
import type { WizardData } from '../Wizard';
import type { WizardSettings } from '../ipc';
import { closeWizard, showMainWindow, updateHotkeys, updateSettings } from '../ipc';

interface StepDoneProps {
  data: WizardData;
}

// Step 6: summary + finish actions. saveWizardSettings reproduces the
// vanilla settings blob and hotkey registration verbatim.
export function StepDone({ data }: StepDoneProps) {
  const save = async () => {
    try {
      const usingOffline = !!data.skipApiKey || data.provider === 'Local Offline';
      const settings: WizardSettings = {
        hotkey: data.hotkey,
        recording_mode: data.recordingMode,
        overlay_position: data.overlayPosition,
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
        theme: 'dark',
        first_run: false,
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
        You&apos;re All Set!
      </h1>
      <p className="step-desc">
        Fluence is running in your system tray. Press{' '}
        <span className="done-hotkey-chip">{data.hotkey}</span> anywhere to start voice typing.
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
          Open Settings
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
          Close Wizard
        </Button>
      </div>
    </>
  );
}
