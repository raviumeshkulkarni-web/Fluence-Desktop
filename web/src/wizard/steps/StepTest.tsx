import { useState } from 'react';
import { Mic, Square } from 'lucide-react';
import { Button } from '@/components/ui/button';
import type { WizardData } from '../Wizard';
import { startRecording, stopRecording, transcribeAudio } from '../ipc';
import { t } from '@/i18n';

interface StepTestProps {
  data: WizardData;
}

type Phase = 'idle' | 'recording' | 'transcribing';

// Step 8: live record → stop → transcribe round-trip. State machine mirrors
// vanilla setupStep5 one-to-one: same labels, same result copy, same
// disabled-while-transcribing, same orb icon swap via the icon-mic/icon-stop
// classes (lucide Mic + filled Square stand in for the vanilla inline SVGs).
export function StepTest({ data }: StepTestProps) {
  const [phase, setPhase] = useState<Phase>('idle');
  const [label, setLabel] = useState(() => t('Start Recording'));
  const [result, setResult] = useState(() => ({
    text: t('Your transcription will appear here…'),
    placeholder: true,
  }));
  const [busy, setBusy] = useState(false);

  const onRecord = async () => {
    if (phase === 'idle') {
      setPhase('recording');
      setLabel(t('Stop Recording'));
      setResult({ text: t('Recording… speak now'), placeholder: false });
      try {
        await startRecording(null);
      } catch (err) {
        setPhase('idle');
        setLabel(t('Start Recording'));
        setResult({
          text: t('Failed to start: {error}', { error: String(err) }),
          placeholder: false,
        });
      }
      return;
    }
    if (phase === 'recording') {
      setPhase('transcribing');
      setLabel(t('Transcribing…'));
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
        setResult({
          text: text || t('(empty transcription)'),
          placeholder: false,
        });
      } catch (err) {
        setResult({
          text: t('Error: {error}', { error: String(err) }),
          placeholder: false,
        });
      } finally {
        setPhase('idle');
        setLabel(t('Try Again'));
        setBusy(false);
      }
    }
  };

  return (
    <>
      <h1 className="step-title" tabIndex={-1}>
        {t('Test Your Setup')}
      </h1>
      <p className="step-desc">{t('Let\'s make sure everything is working. Click the button and say something.')}</p>
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
    </>
  );
}
