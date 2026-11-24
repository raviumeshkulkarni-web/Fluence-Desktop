import { useCallback, useEffect, useRef, useState } from 'react';
import { RefreshCw } from 'lucide-react';
import { ApiKeyInput } from '@/components/fluence/ApiKeyInput';
import { ConnectionStatus } from '@/components/fluence/ConnectionStatus';
import { ProviderIcon } from '@/components/fluence/ProviderIcons';
import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/checkbox';
import { Field } from '@/components/ui/field';
import { Input } from '@/components/ui/input';
import { Progress } from '@/components/ui/progress';
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import type { WizardData, WizardOfflineEngine } from '../Wizard';
import {
  cancelOfflineDownload,
  downloadMoonshineV2SmallModel,
  downloadOfflineModel,
  fetchModels,
  fetchSttModels,
  getApiKey,
  getMoonshineV2SmallModelStatus,
  getOfflineModelStatus,
  subscribeOfflineDownloadProgress,
  testSttConnection,
} from '../ipc';
import { keyTarget } from '@/ipc/providers';
import { describeError } from '@/lib/errors';

const PROVIDER_PRESETS: Record<string, { url: string; model: string; llmModel: string }> = {
  groq: {
    url: 'https://api.groq.com/openai',
    model: 'whisper-large-v3',
    llmModel: 'llama-3.3-70b-versatile',
  },
  openai: { url: 'https://api.openai.com', model: 'whisper-1', llmModel: 'gpt-4o' },
  mistral: { url: 'https://api.mistral.ai', model: 'mistral-stt', llmModel: 'mistral-large-latest' },
  custom: { url: '', model: '', llmModel: '' },
  'Local Offline': { url: '', model: 'moonshine_v2_small', llmModel: '' },
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
  const usingOffline = !!data.skipApiKey || data.provider === 'Local Offline';
  const [sttModels, setSttModels] = useState<string[]>([data.model]);
  const [llmModels, setLlmModels] = useState<string[]>([data.llmModel]);
  const [fetching, setFetching] = useState(false);
  const [test, setTest] = useState<{ kind: 'idle' | 'testing' | 'ok' | 'err'; text: string }>({
    kind: 'idle',
    text: 'Not tested',
  });
  const [downloading, setDownloading] = useState(false);
  const [progress, setProgress] = useState(0);
  const [statusText, setStatusText] = useState('');
  const [dlError, setDlError] = useState<string | null>(null);

  const dataRef = useRef(data);
  dataRef.current = data;
  const onPatchRef = useRef(onPatch);
  onPatchRef.current = onPatch;

  const doFetchModels = useCallback(async (baseUrl: string, apiKey: string) => {
    if (!baseUrl || !apiKey || apiKey.length < 8) return;
    try {
      const u = new URL(baseUrl.trim());
      const loopback =
        u.protocol === 'http:' &&
        ['localhost', '127.0.0.1', '::1'].includes(u.hostname.toLowerCase().replace(/^\[|\]$/g, ''));
      if (u.protocol !== 'https:' && !loopback) return;
    } catch { return; }
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

  // Prefill key when provider changes
  useEffect(() => {
    let cancelled = false;
    const t = window.setTimeout(async () => {
      if (dataRef.current.skipApiKey || dataRef.current.provider === 'Local Offline') return;
      // Prefer the per-preset slot the backend reads; fall back to the legacy
      // global slot so re-running setup on an older install still prefills.
      const presetTarget = keyTarget('stt', dataRef.current.provider);
      let key = await getApiKey(presetTarget).catch(() => null);
      if (!key) key = await getApiKey('Fluence/STT_ApiKey').catch(() => null);
      if (key && !cancelled && !dataRef.current.apiKey) {
        onPatchRef.current({ apiKey: key });
        void doFetchModels(dataRef.current.baseUrl, key);
      }
    }, 500);
    return () => {
      cancelled = true;
      window.clearTimeout(t);
    };
  }, [data.provider, doFetchModels]);

  // Query real install status whenever usingOffline or offlineEngine changes
  useEffect(() => {
    if (!usingOffline) return;
    let cancelled = false;
    const checkStatus = async () => {
      try {
        const engine = dataRef.current.offlineEngine || 'moonshine_v2_small';
        const installed =
          engine === 'moonshine_v2_small'
            ? await getMoonshineV2SmallModelStatus()
            : await getOfflineModelStatus();
        if (!cancelled) {
          onPatchRef.current({ offlineInstalled: installed });
        }
      } catch {
        if (!cancelled) {
          onPatchRef.current({ offlineInstalled: false });
        }
      }
    };
    void checkStatus();
    return () => {
      cancelled = true;
    };
  }, [usingOffline, data.offlineEngine]);

  // Subscribe to offline download progress
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void subscribeOfflineDownloadProgress((p) => {
      if (disposed) return;
      if (p.status === 'downloading' || p.status === 'extracting') {
        setDownloading(true);
        setProgress(p.progress || 0);
        setStatusText(p.currentFile || 'Downloading…');
      } else if (p.status === 'completed') {
        setDownloading(false);
        setProgress(100);
        setStatusText('');
        setDlError(null);
        onError(null);
        onPatchRef.current({ offlineInstalled: true, offlineDownloadSkipped: false });
      } else if (p.status === 'error') {
        setDownloading(false);
        setStatusText('');
        setDlError(describeError(p.errorMessage, 'Download failed. Please try again.'));
      } else if (p.status === 'cancelled') {
        setDownloading(false);
        setStatusText('');
        setDlError('Download cancelled.');
      }
    }).then((u) => {
      if (disposed) u();
      else unlisten = u;
    });

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [onError]);

  const downloadingRef = useRef(false);
  downloadingRef.current = downloading;
  useEffect(() => {
    return () => {
      if (downloadingRef.current) {
        void cancelOfflineDownload().catch(() => undefined);
      }
    };
  }, []);

  const fetchTimer = useRef(0);
  useEffect(() => () => window.clearTimeout(fetchTimer.current), []);

  const onProvider = (preset: string) => {
    if (preset === 'Local Offline') {
      const engine = data.offlineEngine || 'moonshine_v2_small';
      onPatch({
        provider: preset,
        skipApiKey: true,
        apiKey: '',
        baseUrl: '',
        model: engine,
        llmModel: '',
      });
      setSttModels((prev) => withCurrent(prev, engine));
      onError(null);
      setDlError(null);
      return;
    }
    const p = PROVIDER_PRESETS[preset];
    // Never carry a key typed for another provider into the new one.
    onPatch({
      provider: preset,
      skipApiKey: false,
      apiKey: '',
      baseUrl: p.url,
      model: p.model,
      llmModel: p.llmModel,
    });
    setSttModels((prev) => withCurrent(prev, p.model));
    setLlmModels((prev) => withCurrent(prev, p.llmModel));
    onError(null);
    setDlError(null);
  };

  const onOfflineEngineChange = (val: string) => {
    const engine = val as WizardOfflineEngine;
    onPatch({ offlineEngine: engine, model: engine });
    onError(null);
    setDlError(null);
  };

  const onStartDownload = async () => {
    setDlError(null);
    onError(null);
    setDownloading(true);
    setProgress(0);
    setStatusText('Starting download…');
    try {
      const engine = data.offlineEngine || 'moonshine_v2_small';
      if (engine === 'moonshine_v2_small') {
        await downloadMoonshineV2SmallModel();
      } else {
        await downloadOfflineModel();
      }
    } catch (err) {
      setDownloading(false);
      setStatusText('');
      setDlError(describeError(err, 'Failed to start download.'));
    }
  };

  const onCancelDownload = async () => {
    try {
      await cancelOfflineDownload();
    } catch (err) {
      console.error('Cancel download failed:', err);
    }
  };

  const onSkipDownload = () => {
    onPatch({ offlineDownloadSkipped: true });
    setDlError(null);
    onError(null);
    onAdvance();
  };

  const onTest = async () => {
    const baseUrl = dataRef.current.baseUrl.trim();
    const apiKey = dataRef.current.apiKey.trim();
    try {
      const u = new URL(baseUrl);
      const loopback =
        u.protocol === 'http:' &&
        ['localhost', '127.0.0.1', '::1'].includes(u.hostname.toLowerCase().replace(/^\[|\]$/g, ''));
      if (u.protocol !== 'https:' && !loopback) {
        setTest({ kind: 'err', text: 'Use https:// (http only for localhost).' });
        return;
      }
    } catch {
      setTest({ kind: 'err', text: 'Use https:// (http only for localhost).' });
      return;
    }
    setTest({ kind: 'testing', text: 'Testing…' });
    try {
      const msg = await testSttConnection(baseUrl, apiKey);
      setTest({ kind: 'ok', text: msg });
    } catch (err) {
      setTest({ kind: 'err', text: describeError(err) });
    }
  };

  return (
    <>
      <h1 className="step-title" tabIndex={-1}>
        Choose Your Voice Engine
      </h1>
      <p className="step-desc">
        {usingOffline
          ? 'Offline transcription runs on this PC. No API key or account is needed.'
          : 'Fluence uses OpenAI-compatible APIs or local on-device models for voice recognition.'}
      </p>
      <div className="step-content">
        <Field label="Provider">
          <RadioGroup
            className="wizard-provider-grid"
            id="wiz-provider-grid"
            value={data.provider}
            onValueChange={(provider) => onProvider(provider)}
            disabled={downloading}
            aria-label="Provider"
          >
            {PROVIDERS.map((p) => (
              <RadioGroupItem key={p} value={p} asChild>
                <button type="button" data-provider={p} className="choice-surface provider-card">
                  <ProviderIcon preset={p} size={24} className="provider-icon" />
                  <span className="provider-name">{p === 'Local Offline' ? 'Local Offline' : p[0].toUpperCase() + p.slice(1)}</span>
                </button>
              </RadioGroupItem>
            ))}
          </RadioGroup>
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
        {usingOffline ? (
          <div className="wizard-offline-box">
            <Field label="Offline Model">
              <RadioGroup
                className="wizard-offline-grid"
                value={data.offlineEngine || 'moonshine_v2_small'}
                onValueChange={onOfflineEngineChange}
                disabled={downloading}
                aria-label="Offline Engine"
              >
                <RadioGroupItem value="moonshine_v2_small" asChild>
                  <button
                    type="button"
                    className={`choice-surface wizard-offline-engine-card${(data.offlineEngine || 'moonshine_v2_small') === 'moonshine_v2_small' ? ' active' : ''}`}
                  >
                    <div className="wizard-offline-engine-info">
                      <span className="wizard-offline-engine-title">Fast (English, Recommended)</span>
                      <span className="wizard-offline-engine-desc">~142 MB · On-device streaming model</span>
                    </div>
                  </button>
                </RadioGroupItem>
                <RadioGroupItem value="sensevoice" asChild>
                  <button
                    type="button"
                    className={`choice-surface wizard-offline-engine-card${data.offlineEngine === 'sensevoice' ? ' active' : ''}`}
                  >
                    <div className="wizard-offline-engine-info">
                      <span className="wizard-offline-engine-title">Fast (Multilingual)</span>
                      <span className="wizard-offline-engine-desc">~239 MB · EN / ZH / JA / KO on-device model</span>
                    </div>
                  </button>
                </RadioGroupItem>
              </RadioGroup>
            </Field>

            {(error || dlError) && (
              <div className="wizard-offline-error" role="alert">
                {dlError || error}
              </div>
            )}

            {data.offlineInstalled ? (
              <div className="wizard-offline-ready" role="status">
                <span className="wizard-offline-ready-icon" aria-hidden="true">✓</span>
                <span>Model installed and ready on this PC.</span>
              </div>
            ) : downloading ? (
              <div className="wizard-offline-progress">
                <div className="wizard-offline-progress-header">
                  <span>{statusText || 'Downloading model…'}</span>
                  <span>{Math.round(progress)}%</span>
                </div>
                <Progress value={progress} aria-label="Offline model download progress" />
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => void onCancelDownload()}
                  style={{ alignSelf: 'flex-start', marginTop: 4 }}
                >
                  Cancel
                </Button>
              </div>
            ) : (
              <div style={{ display: 'flex', flexDirection: 'column', gap: 8, marginTop: 4 }}>
                <Button
                  onClick={() => void onStartDownload()}
                  style={{ width: '100%', padding: '12px 16px' }}
                >
                  Download Model ({data.offlineEngine === 'sensevoice' ? '~239 MB' : '~142 MB'})
                </Button>
                <div style={{ textAlign: 'center' }}>
                  <button
                    type="button"
                    className="skip-offline"
                    onClick={onSkipDownload}
                  >
                    Skip download for now
                  </button>
                </div>
              </div>
            )}
          </div>
        ) : (
        <>
        <ApiKeyInput
          id="wiz-api-key"
          placeholder="gsk_••••••••••••••••"
          value={data.apiKey}
          error={error ?? undefined}
          onChange={(v) => {
            onPatch({ apiKey: v });
            onError(null);
            window.clearTimeout(fetchTimer.current);
            fetchTimer.current = window.setTimeout(() => {
              void doFetchModels(dataRef.current.baseUrl, v);
            }, 800);
          }}
        />
        <label className="wizard-check-row" htmlFor="wiz-use-agent-key">
          <Checkbox
            id="wiz-use-agent-key"
            checked={data.useKeyForAgent}
            onCheckedChange={(v) => onPatch({ useKeyForAgent: v === true })}
          />
          <span>
            <span className="wizard-check-title">Also use this key for AI Cleanup and Agent Mode</span>
            <span className="wizard-check-hint">
              Uncheck to keep your current AI provider and key.
            </span>
          </span>
        </label>
        <div className="wizard-two-col">
          <Field label="Transcription Model" htmlFor="wiz-model-select">
            <div className="input-with-btn">
              <Select value={data.model} onValueChange={(v) => onPatch({ model: v })}>
                <SelectTrigger id="wiz-model-select">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    {withCurrent(sttModels, data.model).map((m) => (
                      <SelectItem key={m} value={m}>
                        {m}
                      </SelectItem>
                    ))}
                  </SelectGroup>
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
                  <SelectGroup>
                    {withCurrent(llmModels, data.llmModel).map((m) => (
                      <SelectItem key={m} value={m}>
                        {m}
                      </SelectItem>
                    ))}
                  </SelectGroup>
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
          <ConnectionStatus
            kind={test.kind === 'ok' ? 'success' : test.kind === 'err' ? 'error' : 'idle'}
            text={test.text}
          />
        </div>
        <div style={{ marginTop: -4 }}>
          <button
            type="button"
            className="skip-offline"
            onClick={() => onProvider('Local Offline')}
          >
            Continue without a key: use offline transcription
          </button>
        </div>
        </>
        )}
      </div>
    </>
  );
}
