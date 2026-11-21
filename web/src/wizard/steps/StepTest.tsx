import { useEffect, useRef, useState } from 'react';
import { Mic, Square } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { describeError } from '@/lib/errors';
import type { WizardData } from '../Wizard';
import {
  type AppSettings,
  getSettings,
  startRecording,
  stopAndTranscribeRecording,
  stopRecording,
  transcribeAudio,
  updateSettings,
} from '../ipc';

interface StepTestProps {
  data: WizardData;
  active: boolean;
}

type Phase = 'idle' | 'recording' | 'transcribing';

// Step 4: live record → stop → transcribe round-trip.
// Works for both Cloud API and downloaded Local Offline models.
export function StepTest({ data, active }: StepTestProps) {
  const usingOffline = !!data.skipApiKey || data.provider === 'Local Offline';
  const offlineReady = usingOffline && data.offlineInstalled;
  const [phase, setPhase] = useState<Phase>('idle');
  const [label, setLabel] = useState('Start Recording');
  const [result, setResult] = useState({ text: 'Your transcription will appear here…', placeholder: true });
  const [busy, setBusy] = useState(false);
  const phaseRef = useRef<Phase>('idle');
  phaseRef.current = phase;
  const previousSettingsRef = useRef<AppSettings | null>(null);

  // The wizard keeps every step mounted; leaving this step must release the
  // microphone instead of letting the capture run in the background.
  useEffect(() => {
    if (!active) {
      if (previousSettingsRef.current) {
        const prev = previousSettingsRef.current;
        previousSettingsRef.current = null;
        void updateSettings(prev).catch(() => undefined);
      }
      if (phaseRef.current === 'recording') {
        phaseRef.current = 'idle';
        setPhase('idle');
        setLabel('Start Recording');
        setResult({ text: 'Your transcription will appear here…', placeholder: true });
        void stopRecording().catch(() => undefined);
      }
    }
  }, [active]);

  useEffect(() => {
    return () => {
      if (previousSettingsRef.current) {
        const prev = previousSettingsRef.current;
        previousSettingsRef.current = null;
        void updateSettings(prev).catch(() => undefined);
      }
      if (phaseRef.current === 'recording') {
        void stopRecording().catch(() => undefined);
      }
    };
  }, []);

  const onRecord = async () => {
    if (phase === 'idle') {
      setPhase('recording');
      setLabel('Stop Recording');
      setResult({ text: 'Recording… speak now', placeholder: false });
      try {
        if (offlineReady) {
          let previousSettings: AppSettings | null = null;
          previousSettings = await getSettings();
          previousSettingsRef.current = previousSettings;
          await updateSettings({
            ...previousSettings,
            first_run: previousSettings.first_run,
            stt_provider: {
              preset: 'Local Offline',
              base_url: '',
              model: data.offlineEngine,
              api_key_saved: false,
            },
            offline_engine: data.offlineEngine,
          });
        }
        await startRecording(null);
      } catch (err) {
        if (previousSettingsRef.current) {
          const prev = previousSettingsRef.current;
          previousSettingsRef.current = null;
          void updateSettings(prev).catch(() => undefined);
        }
        setPhase('idle');
        setLabel('Start Recording');
        setResult({ text: 'Could not start recording. ' + describeError(err), placeholder: false });
      }
      return;
    }
    if (phase === 'recording') {
      setPhase('transcribing');
      setLabel('Transcribing…');
      setBusy(true);
      try {
        if (offlineReady) {
          const res = await stopAndTranscribeRecording();
          const display =
            res.text ||
            (res.silenceRejected
              ? 'No speech detected: try speaking a little closer to your mic.'
              : '(empty transcription)');
          setResult({ text: display, placeholder: false });
        } else {
          const wavB64 = await stopRecording();
          const text = await transcribeAudio({
            base_url: data.baseUrl,
            api_key: data.apiKey,
            model: data.model,
            wav_b64: wavB64,
            language: 'en',
          });
          setResult({ text: text || '(empty transcription)', placeholder: false });
        }
      } catch (err) {
        setResult({ text: describeError(err, 'The test transcription failed. Please try again.'), placeholder: false });
      } finally {
        if (previousSettingsRef.current) {
          const prev = previousSettingsRef.current;
          previousSettingsRef.current = null;
          void updateSettings(prev).catch(() => undefined);
        }
        setPhase('idle');
        setLabel('Try Again');
        setBusy(false);
      }
    }
  };

  return (
    <>
      <h1 className="step-title" tabIndex={-1}>
        Test Your Setup
      </h1>
      <p className="step-desc">
        {usingOffline && !offlineReady
          ? 'No offline model is downloaded yet. You can download one after setup in Settings.'
          : "Let's make sure everything is working. Click the button and say something."}
      </p>
      {usingOffline && !offlineReady ? (
        <div className="wizard-offline-note" role="status">
          No offline model is downloaded yet. After finishing setup, open Settings → Providers to download Moonshine v2 Small or SenseVoice before using your hotkey.
        </div>
      ) : (
        <div className="test-area">
          <Button
            variant="ghost"
            className={`test-record-btn${phase === 'idle' ? '' : ` ${phase}`}`}
            style={{ minWidth: 160 }}
            disabled={busy}
            onClick={() => void onRecord()}
          >
            <span className="test-record-orb" aria-hidden="true">
              <Mic className="icon-mic test-record-icon" size={22} strokeWidth={2} />
              <Square className="icon-stop" size={11} fill="currentColor" stroke="none" aria-hidden="true" />
            </span>
            <span id="wiz-test-record-label" className="test-record-label">{label}</span>
          </Button>
          <div
            className={`test-result${result.placeholder ? ' placeholder' : ''}`}
            role="status"
            aria-live="polite"
          >
            {result.text}
          </div>
        </div>
      )}
    </>
  );
}
