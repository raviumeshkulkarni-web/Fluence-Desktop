import { useCallback, useEffect, useRef, useState } from 'react';
import { Button } from '@/components/ui/button';
import { Progress } from '@/components/ui/progress';
import { TooltipProvider } from '@/components/ui/tooltip';
import { ConfirmDialog } from '@/components/ui/dialog';
import { listenEvent } from '@/ipc/tauri';
import { keyTarget } from '@/ipc/providers';
import { describeError } from '@/lib/errors';
import { WizardTitlebar } from './WizardTitlebar';
import { cancelOfflineDownload, closeWizard, saveApiKey } from './ipc';
import { StepWelcome } from './steps/StepWelcome';
import { StepApiKey } from './steps/StepApiKey';
import { StepHotkey } from './steps/StepHotkey';
import { StepTest } from './steps/StepTest';
import { StepDone } from './steps/StepDone';

export const TOTAL_STEPS = 5;

export type WizardOfflineEngine = 'moonshine_v2_small' | 'sensevoice';

export interface WizardData {
  provider: string;
  baseUrl: string;
  apiKey: string;
  model: string;
  llmModel: string;
  offlineEngine: WizardOfflineEngine;
  offlineInstalled: boolean;
  offlineDownloadSkipped: boolean;
  hotkey: string;
  recordingMode: string;
  overlayPosition: string;
  overlayStyle: string;
  themeMode: string;
  skipApiKey: boolean;
  useKeyForAgent: boolean;
}

export const DEFAULT_WIZARD_DATA: WizardData = {
  provider: 'groq',
  baseUrl: 'https://api.groq.com/openai',
  apiKey: '',
  model: 'whisper-large-v3',
  llmModel: 'llama-3.3-70b-versatile',
  offlineEngine: 'moonshine_v2_small',
  offlineInstalled: false,
  offlineDownloadSkipped: false,
  hotkey: 'Ctrl+Shift+Space',
  recordingMode: 'push_to_toggle',
  overlayPosition: 'bottom_right',
  overlayStyle: 'full',
  themeMode: 'auto',
  skipApiKey: false,
  useKeyForAgent: true,
};

const STEP_TITLES = [
  '',
  'Welcome to Fluence',
  'Choose Your Voice Engine',
  'Set Your Hotkey',
  'Test Your Setup',
  "You're All Set!",
];

function nextLabel(step: number): string {
  if (step === 1) return 'Get Started';
  if (step === TOTAL_STEPS - 1) return 'Finish Setup';
  return 'Continue';
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
  const [closeConfirm, setCloseConfirm] = useState(false);
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

  // The backend intercepts the wizard's native close (Alt+F4 / taskbar) and
  // re-emits it here so closing always asks instead of hiding silently.
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listenEvent('wizard-close-requested', () => setCloseConfirm(true)).then((u) => {
      if (disposed) u();
      else unlisten = u;
    });
    return () => {
      disposed = true;
      unlisten?.();
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
      // Incoming step always starts at the top — a mid-scroll position must
      // never persist across steps in the fixed-height window.
      document.getElementById(`step-${n}`)?.scrollTo({ top: 0 });
      later(30, () => {
        setEntered(true);
        const el = document.getElementById(`step-${n}`);
        el?.scrollTo({ top: 0 });
        el?.querySelector<HTMLElement>('.step-title')?.focus({ preventScroll: true });
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

  // The theme step previews live: the wizard document follows the pending
  // choice (System tracks the OS while the wizard is open), so what the
  // user picks is what they see. Restores dark on unmount; the saved
  // choice lands in the settings shell via StepDone.
  useEffect(() => {
    const resolve = (): 'dark' | 'light' => {
      if (dataRef.current.themeMode === 'light') return 'light';
      if (dataRef.current.themeMode === 'dark') return 'dark';
      try {
        return window.matchMedia?.('(prefers-color-scheme: light)').matches
          ? 'light'
          : 'dark';
      } catch {
        return 'dark';
      }
    };
    const apply = () => {
      const t = resolve();
      document.documentElement.dataset.theme = t;
      document.documentElement.style.colorScheme = t;
    };
    apply();
    let mq: MediaQueryList | null = null;
    const onChange = () => apply();
    try {
      mq = window.matchMedia?.('(prefers-color-scheme: light)') ?? null;
      mq?.addEventListener('change', onChange);
    } catch {
      mq = null;
    }
    return () => {
      try {
        mq?.removeEventListener('change', onChange);
      } catch {
        // Listener removal is best-effort on teardown.
      }
      document.documentElement.dataset.theme = 'dark';
      document.documentElement.style.colorScheme = 'dark';
    };
  }, [data.themeMode]);

  const flashNext = useCallback(() => {
    setNextGlow(true);
    window.setTimeout(() => setNextGlow(false), 1000);
  }, []);

  const validateAndNext = useCallback(async () => {
    const s = stepRef.current;
    const d = dataRef.current;
    if (s === 2) {
      setApiError(null);
      if (d.skipApiKey || d.provider === 'Local Offline') {
        if (!d.offlineInstalled && !d.offlineDownloadSkipped) {
          setApiError(
            'Download the offline model so voice typing works right away, or click "Skip download for now" below.',
          );
          flashNext();
          return;
        }
      } else {
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
          // Credentials are stored per preset (matches the backend's
          // get_stt_target/get_llm_target lookup); the legacy global slot is
          // only read as a fallback for installs created before this change.
          await saveApiKey(keyTarget('stt', d.provider), key);
          // Saving the same key for AI cleanup / Agent Mode is an explicit
          // choice in StepApiKey; unchecked leaves that provider untouched.
          if (d.useKeyForAgent) await saveApiKey(keyTarget('llm', d.provider), key);
        } catch (err) {
          setApiError('The API key could not be saved. ' + describeError(err));
          flashNext();
          return;
        }
      }
    }
    if (s < TOTAL_STEPS) goTo(s + 1);
  }, [flashNext, goTo, patchData]);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (recordingRef.current || closeConfirm) return;
      const s = stepRef.current;
      if (s < 1 || s > TOTAL_STEPS) return;
      const target = e.target instanceof Element ? e.target : null;
      if (
        target?.closest?.('button, input, select, textarea, a, #wiz-hotkey-display')
      ) {
        return;
      }
      // Up/Down scroll first when the active step overflows; only when the
      // step is already at its edge (or not scrollable) do they move steps.
      // Left/Right always move steps. Keeps keyboard scroll working inside
      // the fixed-height window without breaking the nav contract.
      const scroller = document.getElementById(`step-${s}`);
      const canScroll = scroller ? scroller.scrollHeight > scroller.clientHeight + 1 : false;
      if (e.key === 'ArrowDown' && canScroll && scroller) {
        const atBottom =
          scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 2;
        if (!atBottom) return;
      }
      if (e.key === 'ArrowUp' && canScroll && scroller) {
        if (scroller.scrollTop > 2) return;
      }
      if (e.key === 'ArrowRight' || e.key === 'ArrowDown') {
        e.preventDefault();
        // Forward navigation goes through the same gate as Next/Enter. A bare
        // goTo here walked past the API-key step's validation.
        void validateAndNext();
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
  }, [closeConfirm, goTo, validateAndNext]);

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
      <WizardTitlebar onRequestClose={() => setCloseConfirm(true)} />
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
          {Array.from({ length: TOTAL_STEPS }, (_, k) => k + 1).map((i) => (
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
              {i === 4 && <StepTest data={data} active={step === 4} />}
              {i === 5 && <StepDone data={data} />}
            </section>
          ))}
        </div>
        <div className="wizard-nav">
          <Button
            variant="ghost"
            id="prev-btn"
            style={{ visibility: step > 1 ? 'visible' : 'hidden' }}
            onClick={() => goTo(step - 1)}
          >
            Back
          </Button>
          <div className="step-dots" id="step-dots" aria-hidden="true">
            {Array.from({ length: TOTAL_STEPS }, (_, k) => k + 1).map((i) => (
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
              className={nextGlow ? 'wizard-next-btn is-error-flash' : 'wizard-next-btn'}
              onClick={() => void validateAndNext()}
            >
              {nextLabel(step)}
            </Button>
          )}
        </div>
      </div>
      <ConfirmDialog
        open={closeConfirm}
        onOpenChange={setCloseConfirm}
        title="Skip setup?"
        body="Your choices so far are not saved. Fluence keeps running in the system tray and will show this setup again next time you start it."
        confirmLabel="Skip Setup"
        danger
        onConfirm={() => {
          void cancelOfflineDownload().catch(() => undefined);
          void closeWizard();
        }}
      />
    </div>
    </TooltipProvider>
  );
}
