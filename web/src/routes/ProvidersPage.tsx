import { useCallback, useEffect, useRef, useState } from 'react';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/dialog';
import { Field } from '@/components/ui/field';
import { Input } from '@/components/ui/input';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import * as RadioGroupPrimitive from '@radix-ui/react-radio-group';
import { Progress } from '@/components/ui/progress';
import { toast } from '@/components/fluence/Toasts';
import {
  CustomProviderIcon,
  DownloadIcon,
  GroqIcon,
  MistralIcon,
  OfflineProviderIcon,
  OpenAiIcon,
  RefreshIcon,
} from '@/components/fluence/ProviderIcons';
import {
  getCachedSettings,
  loadSettings,
  saveSettingsNow,
  setSettingField,
} from '@/ipc/settings';
import {
  cancelOfflineDownload,
  deleteMoonshineV2MediumModel,
  deleteMoonshineV2SmallModel,
  deleteOfflineModel,
  downloadMoonshineV2MediumModel,
  downloadMoonshineV2SmallModel,
  downloadOfflineModel,
  fetchModels,
  fetchSttModels,
  getApiKey,
  getMoonshineV2MediumModelStatus,
  getMoonshineV2SmallModelStatus,
  getOfflineModelStatus,
  keyTarget,
  saveApiKey,
  subscribeOfflineDownloadProgress,
  testLlmConnection,
  testSttConnection,
  type OfflineDownloadProgress,
  type ProviderKind,
} from '@/ipc/providers';

const STT_PRESETS: Record<string, { base_url: string; model: string }> = {
  groq: { base_url: 'https://api.groq.com/openai', model: 'whisper-large-v3' },
  openai: { base_url: 'https://api.openai.com', model: 'whisper-1' },
  mistral: { base_url: 'https://api.mistral.ai', model: 'mistral-stt' },
  custom: { base_url: '', model: '' },
  'Local Offline': { base_url: '', model: '' },
};

const LLM_PRESETS: Record<string, { base_url: string; model: string }> = {
  groq: { base_url: 'https://api.groq.com/openai', model: 'llama-3.3-70b-versatile' },
  openai: { base_url: 'https://api.openai.com', model: 'gpt-4o' },
  mistral: { base_url: 'https://api.mistral.ai', model: 'mistral-large-latest' },
  custom: { base_url: '', model: '' },
};

const STT_ORDER = ['groq', 'openai', 'mistral', 'custom', 'Local Offline'];
const LLM_ORDER = ['groq', 'openai', 'mistral', 'custom'];

const STT_NAMES: Record<string, string> = {
  groq: 'Groq',
  openai: 'OpenAI',
  mistral: 'Mistral',
  custom: 'Custom',
  'Local Offline': 'Local Offline',
};

const LLM_NAMES: Record<string, string> = {
  groq: 'Groq',
  openai: 'OpenAI',
  mistral: 'Mistral',
  custom: 'Custom',
};

const STT_IDS: Record<string, string> = {
  groq: 'stt-groq',
  openai: 'stt-openai',
  mistral: 'stt-mistral',
  custom: 'stt-custom',
  'Local Offline': 'stt-offline',
};

const LLM_IDS: Record<string, string> = {
  groq: 'llm-groq',
  openai: 'llm-openai',
  mistral: 'llm-mistral',
  custom: 'llm-custom',
};

function SttIcon({ preset }: { preset: string }) {
  switch (preset) {
    case 'groq': return <GroqIcon />;
    case 'openai': return <OpenAiIcon />;
    case 'mistral': return <MistralIcon />;
    case 'Local Offline': return <OfflineProviderIcon />;
    default: return <CustomProviderIcon />;
  }
}

function LlmIcon({ preset }: { preset: string }) {
  switch (preset) {
    case 'groq': return <GroqIcon />;
    case 'openai': return <OpenAiIcon />;
    case 'mistral': return <MistralIcon />;
    default: return <CustomProviderIcon />;
  }
}

interface EngineCfg {
  engine: string;
  cardId: string;
  title: string;
  desc: string;
  badge?: string;
  speed: number;
  accuracy: number;
  downloadBtnId: string;
  deleteBtnId: string;
  delSize: string;
  delName: string;
  statusCmd: () => Promise<boolean>;
  downloadCmd: () => Promise<void>;
  deleteCmd: () => Promise<number>;
}

const OFFLINE_ENGINES: EngineCfg[] = [
  {
    engine: 'moonshine_v2_small',
    cardId: 'v2small-model-card',
    title: 'Fast (English)',
    desc: 'Quick, reliable English dictation for everyday use (~142 MB)',
    badge: 'Recommended',
    speed: 4,
    accuracy: 4,
    downloadBtnId: 'v2small-download-btn',
    deleteBtnId: 'v2small-delete-btn',
    delSize: '~142 MB',
    delName: 'Fast (English)',
    statusCmd: getMoonshineV2SmallModelStatus,
    downloadCmd: downloadMoonshineV2SmallModel,
    deleteCmd: deleteMoonshineV2SmallModel,
  },
  {
    engine: 'sensevoice',
    cardId: 'sensevoice-model-card',
    title: 'Fast (Multilingual)',
    desc: 'Dictate in many languages with fast, on-device transcription (~239 MB)',
    speed: 5,
    accuracy: 3,
    downloadBtnId: 'offline-download-btn',
    deleteBtnId: 'offline-delete-btn',
    delSize: '~239 MB',
    delName: 'Fast (Multilingual)',
    statusCmd: getOfflineModelStatus,
    downloadCmd: downloadOfflineModel,
    deleteCmd: deleteOfflineModel,
  },
  {
    engine: 'moonshine_v2_medium',
    cardId: 'v2medium-model-card',
    title: 'Pro (English)',
    desc: 'Our most accurate English model. Ideal when every word matters. Slightly slower to respond (~269 MB)',
    speed: 2,
    accuracy: 5,
    downloadBtnId: 'v2medium-download-btn',
    deleteBtnId: 'v2medium-delete-btn',
    delSize: '~269 MB',
    delName: 'Pro (English)',
    statusCmd: getMoonshineV2MediumModelStatus,
    downloadCmd: downloadMoonshineV2MediumModel,
    deleteCmd: deleteMoonshineV2MediumModel,
  },
];

interface FormState {
  preset: string;
  baseUrl: string;
  model: string;
  models: string[];
  apiKey: string;
}

interface ConnStatus {
  dot: string;
  text: string;
}

const IDLE: ConnStatus = { dot: 'dot dot-idle', text: 'Not Tested' };

function formFromSettings(
  provider: Record<string, unknown> | undefined,
  fallbackModel: string,
): FormState {
  const preset =
    typeof provider?.preset === 'string' ? (provider.preset as string) : 'groq';
  const baseUrl =
    typeof provider?.base_url === 'string' ? (provider.base_url as string) : '';
  const model =
    typeof provider?.model === 'string' && provider.model
      ? (provider.model as string)
      : fallbackModel;
  return { preset, baseUrl, model, models: [model], apiKey: '' };
}

// Faithful port of the vanilla Providers surface (#page-providers +
// setupProviderCards/selectProviderCard/fetchModels/testConnection,
// setupOfflineDownloader/updateOfflineStatus, collectProviderSettings,
// setupSaveButtons providers branch). Same DOM ids/classes, same copy,
// same toasts, same debounced auto-apply persistence of the full settings
// object, same explicit Save Changes flush.
export function ProvidersPage() {
  const [stt, setStt] = useState<FormState>(() => ({
    preset: 'groq',
    baseUrl: '',
    model: 'whisper-large-v3',
    models: ['whisper-large-v3'],
    apiKey: '',
  }));
  const [llm, setLlm] = useState<FormState>(() => ({
    preset: 'groq',
    baseUrl: '',
    model: 'llama-3.3-70b-versatile',
    models: ['llama-3.3-70b-versatile'],
    apiKey: '',
  }));
  const [sttStatus, setSttStatus] = useState<ConnStatus>(IDLE);
  const [llmStatus, setLlmStatus] = useState<ConnStatus>(IDLE);
  const [sttFetching, setSttFetching] = useState(false);
  const [llmFetching, setLlmFetching] = useState(false);
  const [engine, setEngine] = useState('sensevoice');
  const [installed, setInstalled] = useState<Record<string, boolean>>({});
  const [downloading, setDownloading] = useState<string | null>(null);
  const [progressVisible, setProgressVisible] = useState(false);
  const [progressStatus, setProgressStatus] = useState('Downloading ASR Engine…');
  const [progressPct, setProgressPct] = useState(0);
  const [progressBytes, setProgressBytes] = useState('0 / 0 MB');
  const [deleteTarget, setDeleteTarget] = useState<EngineCfg | null>(null);

  const sttKeyTimer = useRef<number | null>(null);
  const llmKeyTimer = useRef<number | null>(null);

  const forms = useRef({ stt, llm });
  forms.current = { stt, llm };

  // Never persist when the settings load failed: vanilla's flush is a
  // no-op while currentSettings is null, so a partial write must not
  // clobber the stored file.
  const persistProviders = useCallback((nextStt: FormState, nextLlm: FormState) => {
    if (!getCachedSettings()) return;
    setSettingField('stt_provider', {
      preset: nextStt.preset,
      base_url: nextStt.baseUrl.trim(),
      model: nextStt.model,
      api_key_saved: true,
    });
    setSettingField('llm_provider', {
      preset: nextLlm.preset,
      base_url: nextLlm.baseUrl.trim(),
      model: nextLlm.model,
      api_key_saved: true,
    });
  }, []);

  const setForm = useCallback(
    (kind: ProviderKind, next: FormState) => {
      if (kind === 'stt') {
        setStt(next);
        persistProviders(next, forms.current.llm);
      } else {
        setLlm(next);
        persistProviders(forms.current.stt, next);
      }
    },
    [persistProviders],
  );

  const setModelList = useCallback(
    (kind: ProviderKind, models: string[], current: string) => {
      // Vanilla rebuilds the dropdown from the fetched ids; when the
      // current model is absent the browser falls back to the first
      // option, which is then what a later persist collects.
      const model = models.includes(current) ? current : models[0];
      if (kind === 'stt') {
        setStt((prev) => ({ ...prev, models, model }));
      } else {
        setLlm((prev) => ({ ...prev, models, model }));
      }
    },
    [],
  );

  const doFetchModels = useCallback(
    async (kind: ProviderKind, f: FormState, silent: boolean) => {
      let apiKey = f.apiKey.trim();
      if (!apiKey) {
        apiKey = await getApiKey(keyTarget(kind, f.preset)).catch(() => '');
      }
      const baseUrl = f.baseUrl.trim();
      if (!baseUrl || !apiKey || apiKey.length < 8) {
        if (!silent) toast('Please enter endpoint and API key first', 'error');
        return;
      }
      if (kind === 'stt') setSttFetching(true);
      else setLlmFetching(true);
      try {
        const current = f.model;
        if (kind === 'stt') {
          const res = await fetchSttModels(baseUrl, apiKey, current || null);
          const models = res.models || [];
          if (!models.length) {
            if (!silent) toast('No models found on this endpoint - keeping current list', 'error');
            return;
          }
          setModelList(kind, models, current);
          if (!silent) {
            toast(
              res.filtered !== false
                ? `Loaded ${models.length} models`
                : `Loaded ${models.length} models (unrecognized endpoint - showing all)`,
              'success',
            );
          }
        } else {
          const models = await fetchModels(baseUrl, apiKey);
          if (!models.length) {
            if (!silent) toast('No models found on this endpoint - keeping current list', 'error');
            return;
          }
          setModelList(kind, models, current);
          if (!silent) toast(`Loaded ${models.length} models`, 'success');
        }
      } catch (err) {
        if (!silent) toast('Failed to fetch models: ' + String(err), 'error');
      } finally {
        if (kind === 'stt') setSttFetching(false);
        else setLlmFetching(false);
      }
    },
    [setModelList],
  );

  const refreshOfflineStatus = useCallback(async () => {
    let anyInstalled = false;
    for (const cfg of OFFLINE_ENGINES) {
      try {
        const isInstalled = await cfg.statusCmd();
        if (isInstalled) anyInstalled = true;
        setInstalled((prev) =>
          prev[cfg.engine] === isInstalled ? prev : { ...prev, [cfg.engine]: isInstalled },
        );
      } catch (err) {
        console.error('Failed to get offline model status:', err);
      }
    }
    // Mirrors vanilla hiding the shared progress wrapper once an engine
    // reports installed, and re-arming its download button.
    if (anyInstalled) setProgressVisible(false);
    setDownloading(null);
  }, []);

  // Boot: mirror vanilla loadSettings providers branch + the 500ms
  // delayed stored-key check that silently populates model lists.
  useEffect(() => {
    let cancelled = false;
    const timer = window.setTimeout(() => {
      if (cancelled) return;
      (async () => {
        for (const kind of ['stt', 'llm'] as ProviderKind[]) {
          const f = forms.current[kind];
          const key = await getApiKey(keyTarget(kind, f.preset)).catch(() => null);
          if (cancelled) return;
          if (key) void doFetchModels(kind, f, true);
        }
      })();
    }, 500);
    void (async () => {
      try {
        await loadSettings();
      } catch {
        /* fields keep vanilla defaults until settings arrive */
      }
      if (cancelled) return;
      const s = getCachedSettings();
      const sttProvider = s?.stt_provider as Record<string, unknown> | undefined;
      const llmProvider = s?.llm_provider as Record<string, unknown> | undefined;
      setStt(formFromSettings(sttProvider, 'whisper-large-v3'));
      setLlm(formFromSettings(llmProvider, 'llama-3.3-70b-versatile'));
      const rawEngine =
        typeof s?.offline_engine === 'string' ? (s.offline_engine as string) : 'sensevoice';
      setEngine(
        rawEngine === 'moonshine_base'
          ? 'moonshine_v2_small'
          : OFFLINE_ENGINES.some((c) => c.engine === rawEngine)
            ? rawEngine
            : 'sensevoice',
      );
    })();
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [doFetchModels]);

  // Vanilla shows the downloader + refreshes install states whenever the
  // Local Offline card becomes selected.
  useEffect(() => {
    if (stt.preset === 'Local Offline') void refreshOfflineStatus();
  }, [stt.preset, refreshOfflineStatus]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void subscribeOfflineDownloadProgress((p: OfflineDownloadProgress) =>
      onOfflineProgress(p),
    ).then((u) => {
      unlisten = u;
    });
    return () => unlisten?.();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Ctrl+S quiet save (vanilla saveProviders alias: flush, no toast).
  useEffect(() => {
    const onSave = () => {
      saveSettingsNow().catch((err) =>
        toast('Failed to save settings: ' + String(err), 'error'),
      );
    };
    window.addEventListener('fluence:save-page', onSave);
    return () => window.removeEventListener('fluence:save-page', onSave);
  }, []);

  useEffect(
    () => () => {
      if (sttKeyTimer.current !== null) window.clearTimeout(sttKeyTimer.current);
      if (llmKeyTimer.current !== null) window.clearTimeout(llmKeyTimer.current);
    },
    [],
  );

  const onOfflineProgress = (payload: OfflineDownloadProgress) => {
    const progress = payload.progress;
    const status = payload.status;
    if (status === 'downloading') {
      setProgressVisible(true);
      setProgressStatus(`Downloading: ${payload.currentFile}`);
      setProgressPct(progress);
      const downloadedMb = ((payload.bytesDownloaded ?? 0) / (1024 * 1024)).toFixed(1);
      const totalMb = ((payload.totalBytes ?? 0) / (1024 * 1024)).toFixed(1);
      setProgressBytes(`${downloadedMb} / ${totalMb} MB`);
    } else if (status === 'extracting') {
      setProgressVisible(true);
      setProgressStatus('Extracting model files…');
      setProgressPct(progress);
    } else if (status === 'completed') {
      toast('Offline model downloaded and installed successfully', 'success');
      void refreshOfflineStatus();
    } else if (status === 'error') {
      toast('Offline download failed: ' + payload.errorMessage, 'error');
      void refreshOfflineStatus();
    } else if (status === 'cancelled') {
      toast('Offline download cancelled', 'info');
      void refreshOfflineStatus();
    }
  };

  const selectCard = async (kind: ProviderKind, preset: string) => {
    const presets = kind === 'stt' ? STT_PRESETS : LLM_PRESETS;
    const f = forms.current[kind];
    let next = { ...f, preset, apiKey: '' };
    if (kind === 'stt' ? preset !== 'custom' && preset !== 'Local Offline' : preset !== 'custom') {
      next = {
        ...next,
        baseUrl: presets[preset]?.base_url || '',
        model: presets[preset]?.model || '',
      };
      if (next.model && !next.models.includes(next.model)) {
        next = { ...next, models: [...next.models, next.model] };
      }
    }
    setForm(kind, next);
    const hasKey = await getApiKey(keyTarget(kind, preset))
      .then(() => true)
      .catch(() => false);
    if (hasKey) void doFetchModels(kind, next, true);
  };

  const onKeyInput = (kind: ProviderKind, value: string) => {
    const f = { ...forms.current[kind], apiKey: value };
    setForm(kind, f);
    const timerRef = kind === 'stt' ? sttKeyTimer : llmKeyTimer;
    if (timerRef.current !== null) window.clearTimeout(timerRef.current);
    timerRef.current = window.setTimeout(() => void doFetchModels(kind, f, true), 800);
  };

  const onSaveKey = async (kind: ProviderKind) => {
    const f = forms.current[kind];
    const key = f.apiKey.trim();
    if (!key) {
      toast('Please enter an API key', 'error');
      return;
    }
    try {
      await saveApiKey(keyTarget(kind, f.preset), key);
      setForm(kind, { ...f, apiKey: '' });
      toast(`${f.preset} API key saved securely`, 'success');
    } catch (err) {
      toast('Failed to save key: ' + String(err), 'error');
    }
  };

  const onTest = async (kind: ProviderKind) => {
    const f = forms.current[kind];
    const baseUrl = f.baseUrl.trim();
    const apiKey = await getApiKey(keyTarget(kind, f.preset)).catch(() => '');
    const setStatus = kind === 'stt' ? setSttStatus : setLlmStatus;
    setStatus({ dot: 'dot dot-idle', text: 'Testing…' });
    try {
      const msg =
        kind === 'stt'
          ? await testSttConnection(baseUrl, apiKey)
          : await testLlmConnection(baseUrl, apiKey, f.model);
      setStatus({ dot: 'dot dot-success', text: msg });
    } catch (err) {
      setStatus({ dot: 'dot dot-error', text: String(err).replace('Error: ', '') });
    }
  };

  const onSaveAll = async () => {
    try {
      await saveSettingsNow();
    } catch (err) {
      toast('Failed to save settings: ' + String(err), 'error');
    }
    toast('Provider settings saved', 'success');
  };

  const selectEngine = (name: string) => {
    const target = OFFLINE_ENGINES.some((c) => c.engine === name) ? name : 'sensevoice';
    setEngine(target);
    // Vanilla persists engine picks through the general auto-apply pipe;
    // same debounced full-object write here, skipped while unloaded.
    if (getCachedSettings()) setSettingField('offline_engine', target);
  };

  const onDownload = async (cfg: EngineCfg) => {
    setDownloading(cfg.engine);
    setProgressVisible(true);
    setProgressStatus('Downloading ASR Engine…');
    setProgressPct(0);
    setProgressBytes('0 / 0 MB');
    try {
      await cfg.downloadCmd();
    } catch (err) {
      toast('Failed to start download: ' + String(err), 'error');
      void refreshOfflineStatus();
    }
  };

  const doDeleteModel = async (cfg: EngineCfg) => {
    try {
      const bytesFreed = await cfg.deleteCmd();
      const mbFreed = (bytesFreed / (1024 * 1024)).toFixed(1);
      toast(`${cfg.delName} model deleted. Freed ${mbFreed} MB`, 'success');
      void refreshOfflineStatus();
    } catch (err) {
      toast('Failed to delete model files: ' + String(err), 'error');
    }
  };

  const onCancelDownload = async () => {
    try {
      await cancelOfflineDownload();
    } catch (err) {
      console.error('Failed to cancel download:', err);
    }
  };

  const isOffline = stt.preset === 'Local Offline';

  return (
    <section className="page active" id="page-providers">
      <div className="page-header">
        <h1 className="page-title" tabIndex={-1}>AI Providers</h1>
        <p className="page-subtitle">Configure speech-to-text and language model providers</p>
      </div>

      <div className="settings-section">
        <div className="settings-section-header"><h2>Speech-to-Text (STT)</h2></div>
        <div className="provider-grid" id="stt-provider-grid" role="group" aria-label="Speech-to-text provider">
          {STT_ORDER.map((preset) => {
            const selected = stt.preset === preset;
            return (
              <button
                key={preset}
                className={`provider-card${selected ? ' selected' : ''}`}
                data-provider={preset}
                id={STT_IDS[preset]}
                aria-pressed={selected}
                onClick={() => void selectCard('stt', preset)}
              >
                <SttIcon preset={preset} />
                <span className="provider-name">{STT_NAMES[preset]}</span>
              </button>
            );
          })}
        </div>
        <div style={{ padding: '0 var(--spacing-md) var(--spacing-md)', display: 'flex', flexDirection: 'column', gap: 'var(--spacing-md)' }}>
          <div id="stt-credentials-wrapper" className={isOffline ? 'hidden' : undefined} style={{ display: 'flex', flexDirection: 'column', gap: 'var(--spacing-md)' }}>
            <Field label="API Endpoint" htmlFor="stt-base-url">
              <Input
                type="url"
                id="stt-base-url"
                placeholder="https://api.groq.com/openai"
                value={stt.baseUrl}
                onChange={(e) => setForm('stt', { ...forms.current.stt, baseUrl: e.target.value })}
              />
            </Field>
            <Field label="API Key" htmlFor="stt-api-key">
              <div className="input-with-btn">
                <Input
                  type="password"
                  id="stt-api-key"
                  placeholder="sk-•••••••••••••••"
                  autoComplete="off"
                  value={stt.apiKey}
                  onChange={(e) => onKeyInput('stt', e.target.value)}
                />
                <Button variant="secondary" id="stt-save-key-btn" style={{ whiteSpace: 'nowrap' }} onClick={() => void onSaveKey('stt')}>Save Key</Button>
              </div>
            </Field>
            <Field label="Model" htmlFor="stt-model-select">
              <div className="input-with-btn">
                <Select
                  value={stt.model}
                  onValueChange={(v) => setForm('stt', { ...forms.current.stt, model: v })}
                >
                  <SelectTrigger id="stt-model-select">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {stt.models.map((m) => (
                      <SelectItem key={m} value={m}>{m}</SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <Tooltip>
                  <TooltipTrigger asChild>
                    <Button
                      variant="ghost"
                      id="stt-fetch-models-btn"
                      className={sttFetching ? 'animate-spin' : undefined}
                      aria-label="Fetch speech-to-text models from API"
                      onClick={() => void doFetchModels('stt', forms.current.stt, false)}
                    >
                      <RefreshIcon />
                    </Button>
                  </TooltipTrigger>
                  <TooltipContent>Fetch models from API</TooltipContent>
                </Tooltip>
              </div>
            </Field>
            <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--spacing-md)' }}>
              <Button variant="secondary" id="stt-test-btn" onClick={() => void onTest('stt')}>Test Connection</Button>
              <div className="connection-status" id="stt-status" role="status">
                <div className={sttStatus.dot}></div>
                <span id="stt-status-text">{sttStatus.text}</span>
              </div>
            </div>
          </div>

          <div id="stt-offline-downloader" className={isOffline ? undefined : 'hidden'} style={{ display: 'flex', flexDirection: 'column', gap: 'var(--spacing-md)', marginTop: 4, paddingTop: 8 }}>
            <div style={{ fontWeight: 600, fontSize: 14, color: 'var(--color-on-surface)', display: 'flex', alignItems: 'center', gap: 6 }}>
              <DownloadIcon />
              Offline Model Manager
            </div>
            <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
              <span className="text-body-md" style={{ fontWeight: 600 }}>Choose a model</span>
              <span className="text-muted" style={{ fontSize: 'var(--text-label-sm)' }}>Pick the option that fits how you dictate. You can change this any time.</span>
            </div>
            <RadioGroupPrimitive.Root
              value={engine}
              onValueChange={(v) => selectEngine(v)}
              className="offline-model-list"
              aria-label="Offline speech recognition model"
            >
              {OFFLINE_ENGINES.map((cfg) => {
                const selected = engine === cfg.engine;
                const isInstalled = installed[cfg.engine] === true;
                const isDownloading = downloading === cfg.engine;
                return (
                  <RadioGroupPrimitive.Item
                    key={cfg.engine}
                    asChild
                    value={cfg.engine}
                    onKeyDown={(e) => {
                      if ((e.target as Element).closest('button')) return;
                      if (e.key === 'Enter' || e.key === ' ') {
                        e.preventDefault();
                        selectEngine(cfg.engine);
                      }
                    }}
                  >
                  <div
                    id={cfg.cardId}
                    data-engine={cfg.engine}
                      className={`offline-model-card ui-focus-ring${selected ? ' selected' : ''}`}
                  >
                    <div className="offline-model-card-top">
                      <span className="offline-radio" aria-hidden="true"></span>
                      <span className="text-body-md offline-model-title">{cfg.title}</span>
                      {cfg.badge && <span className="offline-badge">{cfg.badge}</span>}
                    </div>
                    <span className="text-muted offline-model-desc">{cfg.desc}</span>
                    <div className="offline-metrics">
                      <span className="offline-metric" role="img" aria-label={`Speed ${cfg.speed} out of 5`}>
                        <span className="offline-metric-label">Speed</span>
                        <span className="offline-metric-segs">
                          {[1, 2, 3, 4, 5].map((i) => (
                            <i key={i} className={i <= cfg.speed ? 'on' : undefined}></i>
                          ))}
                        </span>
                      </span>
                      <span className="offline-metric" role="img" aria-label={`Accuracy ${cfg.accuracy} out of 5`}>
                        <span className="offline-metric-label">Accuracy</span>
                        <span className="offline-metric-segs">
                          {[1, 2, 3, 4, 5].map((i) => (
                            <i key={i} className={i <= cfg.accuracy ? 'on' : undefined}></i>
                          ))}
                        </span>
                      </span>
                    </div>
                    <div className="offline-model-actions">
                      <Button
                        variant="primary"
                        id={cfg.downloadBtnId}
                        style={{ minWidth: 140 }}
                        disabled={isInstalled || isDownloading}
                        onClick={(e) => {
                          e.stopPropagation();
                          void onDownload(cfg);
                        }}
                      >
                        {isInstalled ? 'Installed' : isDownloading ? 'Connecting…' : 'Download Model'}
                      </Button>
                      <Button
                        variant="danger"
                        id={cfg.deleteBtnId}
                        className={isInstalled ? undefined : 'hidden'}
                        style={{ minWidth: 140 }}
                        onClick={(e) => {
                          e.stopPropagation();
                          setDeleteTarget(cfg);
                        }}
                      >
                        Delete Model
                      </Button>
                    </div>
                  </div>
                  </RadioGroupPrimitive.Item>
                );
              })}
            </RadioGroupPrimitive.Root>
            <div id="offline-progress-wrapper" className={progressVisible ? undefined : 'hidden'} style={{ display: 'flex', flexDirection: 'column', gap: 'var(--spacing-sm)', background: 'var(--color-surface-secondary)', padding: 14, borderRadius: 'var(--radius-md)' }}>
              <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: 12 }}>
                <span id="offline-progress-status" style={{ fontWeight: 500 }}>{progressStatus}</span>
                <span id="offline-progress-percentage" style={{ fontWeight: 600, color: 'var(--color-on-surface)' }}>{progressPct.toFixed(0)}%</span>
              </div>
              <Progress
                value={progressPct}
                aria-label="Offline model download progress"
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={Math.round(progressPct)}
                id="offline-progress-track"
                className="progress-meter"
              />
              <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', fontSize: 11, color: 'var(--color-on-surface-variant)' }}>
                <span id="offline-progress-bytes">{progressBytes}</span>
                <Button variant="ghost" size="xs" id="offline-cancel-btn" onClick={() => void onCancelDownload()}>Cancel</Button>
              </div>
            </div>
          </div>
        </div>
      </div>

      <div className="settings-section">
        <div className="settings-section-header"><h2>Language Model (Agent Mode)</h2></div>
        <div className="provider-grid" id="llm-provider-grid" role="group" aria-label="Language model provider">
          {LLM_ORDER.map((preset) => {
            const selected = llm.preset === preset;
            return (
              <button
                key={preset}
                className={`provider-card${selected ? ' selected' : ''}`}
                data-provider={preset}
                id={LLM_IDS[preset]}
                aria-pressed={selected}
                onClick={() => void selectCard('llm', preset)}
              >
                <LlmIcon preset={preset} />
                <span className="provider-name">{LLM_NAMES[preset]}</span>
              </button>
            );
          })}
        </div>
        <div style={{ padding: '0 var(--spacing-md) var(--spacing-md)', display: 'flex', flexDirection: 'column', gap: 'var(--spacing-md)' }}>
          <Field label="API Endpoint" htmlFor="llm-base-url">
            <Input
              type="url"
              id="llm-base-url"
              placeholder="https://api.groq.com/openai"
              value={llm.baseUrl}
              onChange={(e) => setForm('llm', { ...forms.current.llm, baseUrl: e.target.value })}
            />
          </Field>
          <Field label="API Key" htmlFor="llm-api-key">
            <div className="input-with-btn">
              <Input
                type="password"
                id="llm-api-key"
                placeholder="sk-•••••••••••••••"
                autoComplete="off"
                value={llm.apiKey}
                onChange={(e) => onKeyInput('llm', e.target.value)}
              />
              <Button variant="secondary" id="llm-save-key-btn" style={{ whiteSpace: 'nowrap' }} onClick={() => void onSaveKey('llm')}>Save Key</Button>
            </div>
          </Field>
          <Field label="Model" htmlFor="llm-model-select">
            <div className="input-with-btn">
              <Select
                value={llm.model}
                onValueChange={(v) => setForm('llm', { ...forms.current.llm, model: v })}
              >
                <SelectTrigger id="llm-model-select">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {llm.models.map((m) => (
                    <SelectItem key={m} value={m}>{m}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button
                    variant="ghost"
                    id="llm-fetch-models-btn"
                    className={llmFetching ? 'animate-spin' : undefined}
                    aria-label="Fetch language models from API"
                    onClick={() => void doFetchModels('llm', forms.current.llm, false)}
                  >
                    <RefreshIcon />
                  </Button>
                </TooltipTrigger>
                <TooltipContent>Fetch models from API</TooltipContent>
              </Tooltip>
              </div>
            </Field>
          <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--spacing-md)' }}>
            <Button variant="secondary" id="llm-test-btn" onClick={() => void onTest('llm')}>Test Connection</Button>
            <div className="connection-status" id="llm-status" role="status">
              <div className={llmStatus.dot}></div>
              <span id="llm-status-text">{llmStatus.text}</span>
            </div>
          </div>
        </div>
      </div>

      <div style={{ display: 'flex', justifyContent: 'flex-end', paddingTop: 'var(--spacing-md)' }}>
        <Button variant="primary" id="save-providers-btn" style={{ minWidth: 120 }} onClick={() => void onSaveAll()}>Save Changes</Button>
      </div>

      <ConfirmDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null);
        }}
        title="Delete Model"
        body={
          deleteTarget
            ? `Are you sure you want to delete the ${deleteTarget.delName} model files to free space (${deleteTarget.delSize})?`
            : null
        }
        confirmLabel="Delete Model"
        danger
        onConfirm={() => {
          if (deleteTarget) void doDeleteModel(deleteTarget);
        }}
      />
    </section>
  );
}
