import { useState } from 'react';
import { Mic, Square } from 'lucide-react';
import { Button } from '@/components/ui/button';
import type { WizardData } from '../Wizard';
import { startRecording, stopRecording, transcribeAudio } from '../ipc';

interface StepTestProps {
  data: WizardData;
}

type Phase = 'idle' | 'recording' | 'transcribing';

// Step 5: live record → stop → transcribe round-trip. State machine mirrors
// vanilla setupStep5 one-to-one: same labels, same result copy, same
// disabled-while-transcribing, same orb icon swap via the icon-mic/icon-stop
// classes (lucide Mic + filled Square stand in for the vanilla inline SVGs).
export function StepTest({ data }: StepTestProps) {
  const [phase, setPhase] = useState<Phase>('idle');
  const [label, setLabel] = useState('Start Recording');
  const [result, setResult] = useState({ text: 'Your transcription will appear here…', placeholder: true });
  const [busy, setBusy] = useState(false);

  const onRecord = async () => {
    if (phase === 'idle') {
      setPhase('recording');
      setLabel('Stop Recording');
      setResult({ text: 'Recording… speak now', placeholder: false });
      try {
        await startRecording(null);
      } catch (err) {
        setPhase('idle');
        setLabel('Start Recording');
        setResult({ text: 'Failed to start: ' + String(err), placeholder: false });
      }
      return;
    }
    if (phase === 'recording') {
      setPhase('transcribing');
      setLabel('Transcribing…');
      setBusy(true);
      try {
        const wavB64 = await stopRecording();
        const text = await transcribeAudio({
          base_url: data.baseUrl,
          api_key: data.apiKey,
          model: data.model,
          wav_b64: wavB64,
          language: 'en',
        });
        setResult({ text: text || '(empty transcription)', placeholder: false });
      } catch (err) {
        setResult({ text: 'Error: ' + String(err), placeholder: false });
      } finally {
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
      <p className="step-desc">Let&apos;s make sure everything is working. Click the button and say something.</p>
      <div className="test-area">
        <Button
          className={`test-record-btn${phase === 'idle' ? '' : ` ${phase}`}`}
          style={{ minWidth: 160 }}
          disabled={busy}
          onClick={() => void onRecord()}
        >
          <span className="test-record-orb" aria-hidden="true">
            <Mic className="icon-mic test-record-icon" size={22} strokeWidth={2} />
            <Square className="icon-stop" size={11} fill="currentColor" stroke="none" aria-hidden="true" />
          </span>
          <span id="wiz-test-record-label">{label}</span>
        </Button>
        <div
          className={`test-result${result.placeholder ? ' placeholder' : ''}`}
          role="status"
          aria-live="polite"
        >
          {result.text}
        </div>
      </div>
    </>
  );
}
