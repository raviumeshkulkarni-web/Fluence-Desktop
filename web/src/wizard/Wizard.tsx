import { useCallback, useEffect, useRef, useState } from 'react';
import { Button } from '@/components/ui/button';
import { Progress } from '@/components/ui/progress';
import { TooltipProvider } from '@/components/ui/tooltip';
import { WizardTitlebar } from './WizardTitlebar';
import { saveApiKey } from './ipc';
import { StepWelcome } from './steps/StepWelcome';
import { StepApiKey } from './steps/StepApiKey';
import { StepHotkey } from './steps/StepHotkey';
import { StepPosition } from './steps/StepPosition';
import { StepTest } from './steps/StepTest';
import { StepDone } from './steps/StepDone';

export const TOTAL_STEPS = 6;

export interface WizardData {
  provider: string;
  baseUrl: string;
  apiKey: string;
  model: string;
  llmModel: string;
  hotkey: string;
  recordingMode: string;
  overlayPosition: string;
  skipApiKey: boolean;
}

export const DEFAULT_WIZARD_DATA: WizardData = {
  provider: 'groq',
  baseUrl: 'https://api.groq.com/openai',
  apiKey: '',
  model: 'whisper-large-v3',
  llmModel: 'llama-3.3-70b-versatile',
  hotkey: 'Ctrl+Shift+Space',
  recordingMode: 'push_to_toggle',
  overlayPosition: 'bottom_right',
  skipApiKey: false,
};

const STEP_TITLES = [
  '',
  'Welcome to fluenceTranscribe',
  'Connect Your API Key',
  'Set Your Hotkey',
  'Overlay Position',
  'Test Your Setup',
  "You're All Set!",
];

function nextLabel(step: number): string {
  if (step === 1) return 'Get Started';
  if (step === TOTAL_STEPS - 1) return 'Finish Setup';
  return 'Continue →';
}

// Faithful port of the vanilla wizard shell: same 6-step flow, same
// enter/exit classes with the same 30ms/350ms phasing, same arrow/Enter
// keyboard contract, same progress math, same dots, same nav labels.
// Step bodies arrive in Tasks 23–26; the machine below is final.
export function Wizard() {
  const [step, setStep] = useState(1);
  const [entered, setEntered] = useState(true);
  const [leaving, setLeaving] = useState<{ from: number; dir: 1 | -1 } | null>(null);
  const [data, setData] = useState<WizardData>(DEFAULT_WIZARD_DATA);
  const [apiError, setApiError] = useState<string | null>(null);
  const [nextGlow, setNextGlow] = useState(false);
  const [recordingHotkey, setRecordingHotkey] = useState(false);
  const timers = useRef<number[]>([]);
  const stepRef = useRef(1);
  stepRef.current = step;
  const dataRef = useRef(data);
  dataRef.current = data;
  // Task 24 connects the hotkey recorder here; until then never recording.
  const recordingRef = useRef(false);
  recordingRef.current = recordingHotkey;

  const later = useCallback((ms: number, fn: () => void) => {
    timers.current.push(window.setTimeout(fn, ms));
  }, []);

  useEffect(() => {
    const pending = timers.current;
    return () => {
      pending.forEach((t) => window.clearTimeout(t));
    };
  }, []);

  const goTo = useCallback(
    (n: number) => {
      if (n === stepRef.current || n < 1 || n > TOTAL_STEPS) return;
      timers.current.forEach((t) => window.clearTimeout(t));
      timers.current = [];
      const dir = n > stepRef.current ? 1 : -1;
      setLeaving({ from: stepRef.current, dir });
      setStep(n);
      setEntered(false);
      later(30, () => {
        setEntered(true);
        const el = document.getElementById(`step-${n}`);
        el?.querySelector<HTMLElement>('.step-title')?.focus();
        const announcer = document.getElementById('wiz-step-announcer');
        if (announcer) announcer.textContent = `Step ${n} of ${TOTAL_STEPS}: ${STEP_TITLES[n]}`;
      });
      later(350, () => setLeaving(null));
    },
    [later],
  );

  const patchData = useCallback((patch: Partial<WizardData>) => {
    setData((d) => ({ ...d, ...patch }));
  }, []);

  const flashNext = useCallback(() => {
    setNextGlow(true);
    window.setTimeout(() => setNextGlow(false), 1000);
  }, []);

  const validateAndNext = useCallback(async () => {
    const s = stepRef.current;
    const d = dataRef.current;
    if (s === 2 && !d.skipApiKey && d.provider !== 'Local Offline') {
      const key = d.apiKey.trim();
      const baseUrl = d.baseUrl.trim();
      if (d.provider === 'custom' && !baseUrl) {
        setApiError('Please enter your API endpoint URL');
        flashNext();
        return;
      }
      if (!key) {
        setApiError('Enter your API key to continue, or skip to offline transcription below.');
        flashNext();
        return;
      }
      patchData({ apiKey: key, baseUrl: baseUrl || d.baseUrl });
      try {
        await saveApiKey('Fluence/STT_ApiKey', key);
        await saveApiKey('Fluence/LLM_ApiKey', key);
      } catch (err) {
        setApiError('Failed to save API key: ' + String(err));
        flashNext();
        return;
      }
    }
    if (s < TOTAL_STEPS) goTo(s + 1);
  }, [flashNext, goTo, patchData]);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (recordingRef.current) return;
      const s = stepRef.current;
      if (s < 1 || s > TOTAL_STEPS) return;
      const target = e.target instanceof Element ? e.target : null;
      if (
        target?.closest?.('button, input, select, textarea, a, #wiz-hotkey-display')
      ) {
        return;
      }
      if (e.key === 'ArrowRight' || e.key === 'ArrowDown') {
        e.preventDefault();
        if (s < TOTAL_STEPS) goTo(s + 1);
      } else if (e.key === 'ArrowLeft' || e.key === 'ArrowUp') {
        e.preventDefault();
        if (s > 1) goTo(s - 1);
      } else if (e.key === 'Enter') {
        e.preventDefault();
        void validateAndNext();
      }
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [goTo, validateAndNext]);

  const dir = leaving?.dir ?? 1;
  const stepClass = (i: number): string => {
    const cls = ['wizard-step'];
    if (i === step) cls.push(entered ? 'active' : dir > 0 ? 'enter-right' : 'enter-left');
    if (leaving?.from === i) cls.push(dir > 0 ? 'exit-left' : 'exit-right');
    return cls.join(' ');
  };

  const progress = ((step - 1) / (TOTAL_STEPS - 1)) * 100;

  return (
    <TooltipProvider>
      <div className="wizard-page">
      <WizardTitlebar />
      <div className="wizard-shell" role="dialog" aria-modal="true" aria-label="Fluence setup wizard">
        <div id="wiz-step-announcer" className="sr-only" role="status" aria-live="polite" />
        <Progress
          value={progress}
          aria-label="Setup progress"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={Math.round(progress)}
          style={{ opacity: step === 1 ? 0 : 1 }}
        />
        <div className="wizard-steps">
          {[1, 2, 3, 4, 5, 6].map((i) => (
            <section
              key={i}
              id={`step-${i}`}
              data-step={i}
              className={stepClass(i)}
              aria-current={i === step ? 'step' : undefined}
            >
              {i === 1 && <StepWelcome />}
              {i === 2 && (
                <StepApiKey
                  data={data}
                  onPatch={patchData}
                  error={apiError}
                  onError={setApiError}
                  onAdvance={() => goTo(stepRef.current + 1)}
                />
              )}
              {i === 3 && (
                <StepHotkey
                  data={data}
                  onPatch={patchData}
                  active={step === 3}
                  onRecordingChange={setRecordingHotkey}
                />
              )}
              {i === 4 && <StepPosition data={data} onPatch={patchData} />}
              {i === 5 && <StepTest data={data} />}
              {i === 6 && <StepDone data={data} />}
            </section>
          ))}
        </div>
        <div className="wizard-nav">
          <Button
            variant="ghost"
            id="prev-btn"
            style={{ visibility: step > 1 && step < TOTAL_STEPS ? 'visible' : 'hidden' }}
            onClick={() => goTo(step - 1)}
          >
            ← Back
          </Button>
          <div className="step-dots" id="step-dots" aria-hidden="true">
            {[1, 2, 3, 4, 5, 6].map((i) => (
              <div
                key={i}
                data-dot={i}
                className={`step-dot${i === step ? ' active' : i < step ? ' done' : ''}`}
              />
            ))}
          </div>
          {step < TOTAL_STEPS && (
            <Button
              id="next-btn"
              style={{
                minWidth: 120,
                boxShadow: nextGlow ? '0 0 0 3px rgba(255,100,100,0.4)' : undefined,
              }}
              onClick={() => void validateAndNext()}
            >
              {nextLabel(step)}
            </Button>
          )}
        </div>
      </div>
    </div>
    </TooltipProvider>
  );
}
