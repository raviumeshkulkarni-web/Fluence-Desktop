import { useCallback, useEffect, useRef, useState } from 'react';
import { RefreshCw } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Field } from '@/components/ui/field';
import { Input } from '@/components/ui/input';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import type { WizardData } from '../Wizard';
import { fetchModels, fetchSttModels, getApiKey, testSttConnection } from '../ipc';

const PROVIDER_PRESETS: Record<string, { url: string; model: string; llmModel: string }> = {
  groq: {
    url: 'https://api.groq.com/openai',
    model: 'whisper-large-v3',
    llmModel: 'llama-3.3-70b-versatile',
  },
  openai: { url: 'https://api.openai.com', model: 'whisper-1', llmModel: 'gpt-4o' },
  mistral: { url: 'https://api.mistral.ai', model: 'mistral-stt', llmModel: 'mistral-large-latest' },
  custom: { url: '', model: '', llmModel: '' },
  'Local Offline': { url: '', model: 'sensevoice', llmModel: '' },
};

const PROVIDERS = ['groq', 'openai', 'mistral', 'custom', 'Local Offline'] as const;

function withCurrent(list: string[], cur: string): string[] {
  if (!cur) return list;
  return list.includes(cur) ? list : [...list, cur];
}

interface StepApiKeyProps {
  data: WizardData;
  onPatch: (patch: Partial<WizardData>) => void;
  error: string | null;
  onError: (msg: string | null) => void;
  onAdvance: () => void;
}

// Faithful port of vanilla setupStep2 + the step-2 branch of
// validateCurrentStep: same presets, same fetch/test/skip flows, same copy.
// Empty-string model values are never rendered as Select items (Radix
// rejects them); the trigger then shows blank exactly as vanilla's
// appended empty <option> does.
export function StepApiKey({ data, onPatch, error, onError, onAdvance }: StepApiKeyProps) {
  const [sttModels, setSttModels] = useState<string[]>([data.model]);
  const [llmModels, setLlmModels] = useState<string[]>([data.llmModel]);
  const [fetching, setFetching] = useState(false);
  const [test, setTest] = useState<{ kind: 'idle' | 'testing' | 'ok' | 'err'; text: string }>({
    kind: 'idle',
    text: 'Not tested',
  });
  const dataRef = useRef(data);
  dataRef.current = data;
  const onPatchRef = useRef(onPatch);
  onPatchRef.current = onPatch;

  const doFetchModels = useCallback(async (baseUrl: string, apiKey: string) => {
    if (!baseUrl || !apiKey || apiKey.length < 8) return;
    setFetching(true);
    try {
      const keep = dataRef.current.model || null;
      const sttRes = await fetchSttModels(baseUrl, apiKey, keep);
      const models = await fetchModels(baseUrl, apiKey);
      const stt = (sttRes.models ?? []).filter(Boolean);
      const llm = (models ?? []).filter(Boolean);
      if (stt.length) {
        setSttModels(stt);
        if (dataRef.current.model && !stt.includes(dataRef.current.model)) {
          onPatchRef.current({ model: stt[0] });
        }
      }
      if (llm.length) {
        setLlmModels(llm);
        if (dataRef.current.llmModel && !llm.includes(dataRef.current.llmModel)) {
          onPatchRef.current({ llmModel: llm[0] });
        }
      }
    } catch (err) {
      console.error('Fetch models failed:', err);
    } finally {
      setFetching(false);
    }
  }, []);

  useEffect(() => {
    const t = window.setTimeout(async () => {
      const key = await getApiKey('Fluence/STT_ApiKey').catch(() => null);
      if (key) {
        onPatchRef.current({ apiKey: key });
        void doFetchModels(dataRef.current.baseUrl, key);
      }
    }, 500);
    return () => window.clearTimeout(t);
  }, [doFetchModels]);

  const fetchTimer = useRef(0);
  useEffect(() => () => window.clearTimeout(fetchTimer.current), []);

  const onProvider = (preset: string) => {
    if (preset === 'Local Offline') {
      onPatch({ provider: preset, skipApiKey: true, baseUrl: '', model: 'sensevoice', llmModel: '' });
      setSttModels((prev) => withCurrent(prev, 'sensevoice'));
      onError(null);
      return;
    }
    const p = PROVIDER_PRESETS[preset];
    onPatch({
      provider: preset,
      skipApiKey: false,
      baseUrl: p.url,
      model: p.model,
      llmModel: p.llmModel,
    });
    setSttModels((prev) => withCurrent(prev, p.model));
    setLlmModels((prev) => withCurrent(prev, p.llmModel));
  };

  const onTest = async () => {
    const baseUrl = dataRef.current.baseUrl.trim();
    const apiKey = dataRef.current.apiKey.trim();
    setTest({ kind: 'testing', text: 'Testing…' });
    try {
      const msg = await testSttConnection(baseUrl, apiKey);
      setTest({ kind: 'ok', text: msg });
    } catch (err) {
      setTest({ kind: 'err', text: String(err).replace('Error: ', '') });
    }
  };

  const onSkip = () => {
    onPatch({ skipApiKey: true, provider: 'Local Offline', baseUrl: '' });
    onError(null);
    onAdvance();
  };

  return (
    <>
      <h1 className="step-title" tabIndex={-1}>
        Connect Your API Key
      </h1>
      <p className="step-desc">
        Fluence uses OpenAI-compatible APIs for voice recognition. Add your API key to get started.
      </p>
      <div className="step-content">
        <Field label="Provider" htmlFor="wiz-provider-grid">
          <div className="wizard-provider-grid" role="group" aria-label="Provider" id="wiz-provider-grid">
            {PROVIDERS.map((p) => (
              <button
                key={p}
                type="button"
                data-provider={p}
                aria-pressed={data.provider === p}
                className={`provider-card${data.provider === p ? ' selected' : ''}`}
                onClick={() => onProvider(p)}
              >
                <ProviderIcon preset={p} />
                <span className="provider-name">{p === 'Local Offline' ? 'Local Offline' : p[0].toUpperCase() + p.slice(1)}</span>
              </button>
            ))}
          </div>
        </Field>
        {data.provider === 'custom' && (
          <Field label="API Endpoint" htmlFor="wiz-base-url">
            <Input
              id="wiz-base-url"
              type="url"
              placeholder="https://api.groq.com/openai"
              value={data.baseUrl}
              onChange={(e) => onPatch({ baseUrl: e.target.value })}
            />
          </Field>
        )}
        <Field label="API Key" htmlFor="wiz-api-key" error={error ?? undefined}>
          <div className="input-with-btn">
            <Input
              id="wiz-api-key"
              type="password"
              placeholder="gsk_••••••••••••••••"
              autoComplete="off"
              value={data.apiKey}
              onChange={(e) => {
                onPatch({ apiKey: e.target.value });
                onError(null);
                window.clearTimeout(fetchTimer.current);
                const v = e.target.value;
                fetchTimer.current = window.setTimeout(() => {
                  void doFetchModels(dataRef.current.baseUrl, v);
                }, 800);
              }}
            />
          </div>
        </Field>
        <div className="wizard-two-col">
          <Field label="Transcription Model" htmlFor="wiz-model-select">
            <div className="input-with-btn">
              <Select value={data.model} onValueChange={(v) => onPatch({ model: v })}>
                <SelectTrigger id="wiz-model-select">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {withCurrent(sttModels, data.model).map((m) => (
                    <SelectItem key={m} value={m}>
                      {m}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button
                    variant="ghost"
                    aria-label="Fetch transcription models from API"
                    style={{ flexShrink: 0 }}
                    className={fetching ? 'animate-spin' : undefined}
                    onClick={() => void doFetchModels(dataRef.current.baseUrl, dataRef.current.apiKey)}
                  >
                    <RefreshCw size={16} strokeWidth={2} aria-hidden="true" />
                  </Button>
                </TooltipTrigger>
                <TooltipContent>Fetch models</TooltipContent>
              </Tooltip>
            </div>
          </Field>
          <Field label="Agent Mode Model" htmlFor="wiz-llm-model-select">
            <div className="input-with-btn">
              <Select value={data.llmModel} onValueChange={(v) => onPatch({ llmModel: v })}>
                <SelectTrigger id="wiz-llm-model-select">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {withCurrent(llmModels, data.llmModel).map((m) => (
                    <SelectItem key={m} value={m}>
                      {m}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button
                    variant="ghost"
                    aria-label="Fetch language models from API"
                    style={{ flexShrink: 0 }}
                    className={fetching ? 'animate-spin' : undefined}
                    onClick={() => void doFetchModels(dataRef.current.baseUrl, dataRef.current.apiKey)}
                  >
                    <RefreshCw size={16} strokeWidth={2} aria-hidden="true" />
                  </Button>
                </TooltipTrigger>
                <TooltipContent>Fetch LLM models</TooltipContent>
              </Tooltip>
            </div>
          </Field>
        </div>
        <div className="wizard-test-row">
          <Button variant="secondary" style={{ padding: '8px 16px' }} onClick={() => void onTest()}>
            Test Connection
          </Button>
          <div style={{ display: 'flex', alignItems: 'center', gap: 6 }} role="status">
            <div className={`dot dot-${test.kind === 'testing' ? 'idle' : test.kind === 'ok' ? 'success' : test.kind === 'err' ? 'error' : 'idle'}`} />
            <span style={{ fontSize: 'var(--text-label-lg)', color: 'var(--color-on-surface-variant)' }}>
              {test.text}
            </span>
          </div>
        </div>
        <div style={{ marginTop: -4 }}>
          <button type="button" className="skip-offline" onClick={onSkip}>
            Continue without a key: use offline transcription
          </button>
        </div>
      </div>
    </>
  );
}

function ProviderIcon({ preset }: { preset: string }) {
  if (preset === 'groq') {
    return (
      <svg className="provider-icon groq-icon" width="24" height="24" viewBox="0 0 100 100" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
        <rect width="100" height="100" rx="16" fill="#F55036" />
        <path d="M53 20 C 37 20 28 30 28 45 C 28 60 37 70 53 70 C 58 70 63 68 67 65 L 67 71 C 67 82 59 90 47 90 C 40 90 35 87 31 81 L 22 88 C 28 97 37 100 47 100 C 65 100 78 88 78 71 L 78 22 L 67 22 L 67 27 C 63 23 58 20 53 20 Z M 53 60 C 44 60 39 53 39 45 C 39 37 44 30 53 30 C 62 30 67 37 67 45 C 67 53 62 60 53 60 Z" fill="#fff" />
      </svg>
    );
  }
  if (preset === 'openai') {
    return (
      <svg className="provider-icon openai-icon" width="24" height="24" viewBox="0 0 24 24" fill="currentColor" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
        <path d="M22.282 9.821a5.985 5.985 0 0 0-.516-4.91 6.046 6.046 0 0 0-6.51-2.9A6.065 6.065 0 0 0 4.981 4.18a5.985 5.985 0 0 0-3.998 2.9 6.046 6.046 0 0 0 .743 7.097 5.98 5.98 0 0 0 .51 4.911 6.051 6.051 0 0 0 6.515 2.9A5.985 5.985 0 0 0 13.26 24a6.056 6.056 0 0 0 5.772-4.206 5.99 5.99 0 0 0 3.997-2.9 6.056 6.056 0 0 0-.747-7.073zM13.26 22.43a4.476 4.476 0 0 1-2.876-1.04l.141-.081 4.779-2.758a.795.795 0 0 0 .392-.681v-6.737l2.02 1.168a.071.071 0 0 1 .038.052v5.583a4.504 4.504 0 0 1-4.494 4.494zM3.6 18.304a4.47 4.47 0 0 1-.535-3.014l.142.085 4.783 2.759a.771.771 0 0 0 .78 0l5.843-3.369v2.332a.08.08 0 0 1-.033.062L9.74 19.95a4.5 4.5 0 0 1-6.14-1.646zM2.34 7.896a4.485 4.485 0 0 1 2.366-1.973V11.6a.766.766 0 0 0 .388.676l5.815 3.355-2.02 1.168a.076.076 0 0 1-.071 0l-4.83-2.786A4.504 4.504 0 0 1 2.34 7.896zm16.597 3.855l-5.843-3.372 2.02-1.163a.076.076 0 0 1 .071 0l4.83 2.786a4.494 4.494 0 0 1-.676 8.105v-5.678a.79.79 0 0 0-.402-.678zm2.01-3.023l-.141-.085-4.774-2.782a.776.776 0 0 0-.785 0L9.409 9.23V6.897a.066.066 0 0 1 .028-.061l4.83-2.787a4.5 4.5 0 0 1 6.68 4.66zm-12.64 4.135l-2.02-1.164a.08.08 0 0 1-.038-.057V6.075a4.5 4.5 0 0 1 7.375-3.453l-.142.08L8.704 5.46a.795.795 0 0 0-.393.681zm1.097-2.365l2.602-1.5 2.607 1.5v2.999l-2.597 1.5-2.607-1.5z" />
      </svg>
    );
  }
  if (preset === 'mistral') {
    return (
      <svg className="provider-icon mistral-icon" width="24" height="24" viewBox="0 0 50 50" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
        <rect x="0" y="0" width="10" height="10" fill="#FCD34D" />
        <rect x="40" y="0" width="10" height="10" fill="#FCD34D" />
        <rect x="0" y="10" width="10" height="10" fill="#F59E0B" />
        <rect x="40" y="10" width="10" height="10" fill="#F59E0B" />
        <rect x="0" y="20" width="50" height="10" fill="#F97316" />
        <rect x="0" y="30" width="10" height="10" fill="#EA580C" />
        <rect x="20" y="30" width="10" height="10" fill="#EA580C" />
        <rect x="40" y="30" width="10" height="10" fill="#EA580C" />
        <rect x="0" y="40" width="20" height="10" fill="#DC2626" />
        <rect x="30" y="40" width="20" height="10" fill="#DC2626" />
      </svg>
    );
  }
  if (preset === 'custom') {
    return (
      <svg className="provider-icon custom-icon" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <circle cx="12" cy="12" r="3" />
        <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
      </svg>
    );
  }
  return (
    <svg className="provider-icon offline-icon" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
      <path d="M12 2L2 7l10 5 10-5-10-5zM2 17l10 5 10-5M2 12l10 5 10-5" />
    </svg>
  );
}
