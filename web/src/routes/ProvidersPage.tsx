import { useCallback, useEffect, useRef, useState } from 'react';
import { Button } from '@/components/ui/button';
import { SettingsSectionHeader } from '@/components/fluence/SettingsSection';
import { ConfirmDialog } from '@/components/ui/dialog';
import { Field } from '@/components/ui/field';
import { Input } from '@/components/ui/input';
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import { Progress } from '@/components/ui/progress';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group';
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

interface LlmSectionProps {
  kind: 'llm' | 'cleaner';
  form: FormState;
  status: ConnStatus;
  fetching: boolean;
  idPrefix: string;
  title: string;
  description: string;
  onSelectCard: (kind: ProviderKind, preset: string) => void;
  onSetForm: (kind: ProviderKind, next: FormState) => void;
  onKeyInput: (kind: ProviderKind, value: string) => void;
  onSaveKey: (kind: ProviderKind) => void;
  onTest: (kind: ProviderKind) => void;
  onFetchModels: (kind: ProviderKind, f: FormState, silent: boolean) => void;
}

// Language-model provider form shared by Agent Mode and AI cleaner: same
// provider cards, endpoint/key/model rows, model fetch, and connection
// test. Only the settings slot (llm_provider vs cleaner_provider) and the
// DOM id prefix differ.
function LlmProviderSection({
  kind,
  form,
  status,
  fetching,
  idPrefix,
  title,
  description,
  onSelectCard,
  onSetForm,
  onKeyInput,
  onSaveKey,
  onTest,
  onFetchModels,
}: LlmSectionProps) {
  return (
    <>
      <SettingsSectionHeader title={title} description={description} />
      <div className="settings-card provider-card-container">
        <RadioGroup
          className="provider-grid"
          id={`${idPrefix}-provider-grid`}
          value={form.preset}
          onValueChange={(preset) => void onSelectCard(kind, preset)}
          aria-label={title}
        >
          {LLM_ORDER.map((preset) => (
            <RadioGroupItem key={preset} value={preset} asChild>
              <button
                type="button"
                className={`choice-surface provider-card${form.preset === preset ? ' selected' : ''}`}
                data-provider={preset}
                id={`${idPrefix}-${preset}`}
              >
                <LlmIcon preset={preset} />
                <span className="provider-name">{LLM_NAMES[preset]}</span>
              </button>
            </RadioGroupItem>
          ))}
        </RadioGroup>
        <div className="provider-form-fields">
          <Field label="API Endpoint" htmlFor={`${idPrefix}-base-url`}>
            <Input
              type="url"
              id={`${idPrefix}-base-url`}
              placeholder="https://api.groq.com/openai"
              value={form.baseUrl}
              onChange={(e) => onSetForm(kind, { ...form, baseUrl: e.target.value })}
            />
            {isCustomHttpsEndpoint(form.baseUrl) && (
              <p className="field-warning">Custom endpoint: prompts, context and bearer credentials may be sent to this server.</p>
            )}
          </Field>
          <Field label="API Key" htmlFor={`${idPrefix}-api-key`}>
            <div className="input-with-btn">
              <Input
                type="password"
                id={`${idPrefix}-api-key`}
                placeholder="sk-•••••••••••••••"
                autoComplete="off"
                value={form.apiKey}
                onChange={(e) => onKeyInput(kind, e.target.value)}
              />
              <Button variant="secondary" id={`${idPrefix}-save-key-btn`} className="provider-save-key" onClick={() => void onSaveKey(kind)}>Save Key</Button>
            </div>
          </Field>
          <Field label="Model" htmlFor={`${idPrefix}-model-select`}>
            <div className="input-with-btn">
              <Select
                value={form.model}
                onValueChange={(v) => onSetForm(kind, { ...form, model: v })}
              >
                <SelectTrigger id={`${idPrefix}-model-select`}>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    {form.models.map((m) => (
                      <SelectItem key={m} value={m}>{m}</SelectItem>
                    ))}
                  </SelectGroup>
                </SelectContent>
              </Select>
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button
                    variant="ghost"
                    id={`${idPrefix}-fetch-models-btn`}
                    className={fetching ? 'animate-spin' : undefined}
                    aria-label="Fetch language models from API"
                    onClick={() => void onFetchModels(kind, form, false)}
                  >
                    <RefreshIcon />
                  </Button>
                </TooltipTrigger>
                <TooltipContent>Fetch models from API</TooltipContent>
              </Tooltip>
            </div>
          </Field>
          <div className="provider-test-row">
            <Button variant="secondary" id={`${idPrefix}-test-btn`} onClick={() => void onTest(kind)}>Test Connection</Button>
            <div className="connection-status" id={`${idPrefix}-status`} role="status">
              <div className={status.dot}></div>
              <span id={`${idPrefix}-status-text`}>{status.text}</span>
            </div>
          </div>
        </div>
      </div>
    </>
  );
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

function isAllowedEndpointUrl(raw: string): boolean {
  try {
    const u = new URL(String(raw || '').trim());
    if (u.protocol === 'https:') return true;
    if (u.protocol === 'http:') {
      const host = String(u.hostname || '').toLowerCase().replace(/^\[|\]$/g, '');
      return host === 'localhost' || host === '127.0.0.1' || host === '::1';
    }
    return false;
  } catch { return false; }
}

function isCustomHttpsEndpoint(raw: string): boolean {
  try {
    const u = new URL(String(raw || '').trim());
    if (u.protocol !== 'https:') return false;
    const host = String(u.hostname || '').toLowerCase();
    return !host.endsWith('groq.com') && !host.endsWith('openai.com') && !host.endsWith('mistral.ai');
  } catch { return false; }
}

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

// One tab per model job: dictation (STT), Agent Mode (LLM), AI cleaner
// (dedicated polish provider split out of the LLM slot). Only the active
// tab's section renders; all three persist through the same debounced
// auto-apply pipe, same explicit Save Changes flush.
type ProvidersTab = 'dictation' | 'agent' | 'cleaner';

const TAB_STORAGE_KEY = 'fluence:providers-tab';

function readInitialTab(): ProvidersTab {
  try {
    const v = window.sessionStorage.getItem(TAB_STORAGE_KEY);
    if (v === 'agent' || v === 'cleaner' || v === 'dictation') {
      window.sessionStorage.removeItem(TAB_STORAGE_KEY);
      return v;
    }
  } catch {
    // Storage is an enhancement; default tab still works without it.
  }
  return 'dictation';
}

// Faithful port of the vanilla Providers surface (#page-providers +
// setupProviderCards/selectProviderCard/fetchModels/testConnection,
// setupOfflineDownloader/updateOfflineStatus, collectProviderSettings,
// setupSaveButtons providers branch). Same DOM ids/classes, same copy,
// same toasts, same debounced auto-apply persistence of the full settings
// object, same explicit Save Changes flush.
export function ProvidersPage() {
  const [tab, setTab] = useState<ProvidersTab>(readInitialTab);
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
  const [cleaner, setCleaner] = useState<FormState>(() => ({
    preset: 'groq',
    baseUrl: '',
    model: 'llama-3.3-70b-versatile',
    models: ['llama-3.3-70b-versatile'],
    apiKey: '',
  }));
  const [sttStatus, setSttStatus] = useState<ConnStatus>(IDLE);
  const [llmStatus, setLlmStatus] = useState<ConnStatus>(IDLE);
  const [cleanerStatus, setCleanerStatus] = useState<ConnStatus>(IDLE);
  const [sttFetching, setSttFetching] = useState(false);
  const [llmFetching, setLlmFetching] = useState(false);
  const [cleanerFetching, setCleanerFetching] = useState(false);
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
  const cleanerKeyTimer = useRef<number | null>(null);

  const forms = useRef({ stt, llm, cleaner });
  forms.current = { stt, llm, cleaner };

  // Never persist when the settings load failed: vanilla's flush is a
  // no-op while currentSettings is null, so a partial write must not
  // clobber the stored file.
  const persistProviders = useCallback(
    (nextStt: FormState, nextLlm: FormState, nextCleaner: FormState) => {
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
      setSettingField('cleaner_provider', {
        preset: nextCleaner.preset,
        base_url: nextCleaner.baseUrl.trim(),
        model: nextCleaner.model,
        api_key_saved: true,
      });
    },
    [],
  );

  const setForm = useCallback(
    (kind: ProviderKind, next: FormState) => {
      if (kind === 'stt') {
        setStt(next);
        persistProviders(next, forms.current.llm, forms.current.cleaner);
      } else if (kind === 'llm') {
        setLlm(next);
        persistProviders(forms.current.stt, next, forms.current.cleaner);
      } else {
        setCleaner(next);
        persistProviders(forms.current.stt, forms.current.llm, next);
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
      } else if (kind === 'llm') {
        setLlm((prev) => ({ ...prev, models, model }));
      } else {
        setCleaner((prev) => ({ ...prev, models, model }));
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
      if (!isAllowedEndpointUrl(baseUrl)) {
        if (!silent) toast('Use https:// (http only for localhost).', 'error');
        return;
      }
      if (kind === 'stt') setSttFetching(true);
      else if (kind === 'llm') setLlmFetching(true);
      else setCleanerFetching(true);
      try {
        const current = f.model;
        if (kind === 'stt') {
          const res = await fetchSttModels(baseUrl, apiKey, current || null);
          const models = res.models || [];
          if (!models.length) {
            if (!silent) toast('No models found on this endpoint, keeping current list', 'error');
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
            if (!silent) toast('No models found on this endpoint, keeping current list', 'error');
            return;
          }
          setModelList(kind, models, current);
          if (!silent) toast(`Loaded ${models.length} models`, 'success');
        }
      } catch (err) {
        if (!silent) toast('Failed to fetch models: ' + String(err), 'error');
      } finally {
        if (kind === 'stt') setSttFetching(false);
        else if (kind === 'llm') setLlmFetching(false);
        else setCleanerFetching(false);
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
        for (const kind of ['stt', 'llm', 'cleaner'] as ProviderKind[]) {
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
      const cleanerProvider = s?.cleaner_provider as Record<string, unknown> | undefined;
      setStt(formFromSettings(sttProvider, 'whisper-large-v3'));
      setLlm(formFromSettings(llmProvider, 'llama-3.3-70b-versatile'));
      // Backend migrates old files (cleaner inherits the LLM provider), so
      // by the time the cache lands this is either the user's pick or that
      // inherited value — never a surprise reset.
      setCleaner(formFromSettings(cleanerProvider, 'llama-3.3-70b-versatile'));
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

  // Cross-page deep link (e.g. AI Post Processing → AI cleaner tab):
  // pages dispatch `fluence:providers-tab` and/or stash the tab in
  // session storage (read once in readInitialTab for the fresh-mount
  // case, where this listener is not yet attached).
  useEffect(() => {
    const onTab = (e: Event) => {
      const v = (e as CustomEvent<unknown>).detail;
      if (v === 'dictation' || v === 'agent' || v === 'cleaner') setTab(v);
    };
    window.addEventListener('fluence:providers-tab', onTab);
    return () => window.removeEventListener('fluence:providers-tab', onTab);
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
      if (cleanerKeyTimer.current !== null) window.clearTimeout(cleanerKeyTimer.current);
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
    const timerRef = kind === 'stt' ? sttKeyTimer : kind === 'llm' ? llmKeyTimer : cleanerKeyTimer;
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
    const setStatus =
      kind === 'stt' ? setSttStatus : kind === 'llm' ? setLlmStatus : setCleanerStatus;
    if (!isAllowedEndpointUrl(baseUrl)) {
      setStatus({ dot: 'dot dot-error', text: 'Use https:// (http only for localhost).' });
      return;
    }
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
      return;
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
        <h1 className="page-title" tabIndex={-1}>Providers</h1>
        <p className="page-subtitle">Configure dictation, agent mode, and AI cleanup</p>
      </div>

      <Tabs className="page-tabs" value={tab} onValueChange={(v) => setTab(v as ProvidersTab)}>
        <TabsList aria-label="Provider area">
          <TabsTrigger value="dictation">Dictation</TabsTrigger>
          <TabsTrigger value="agent">Agent mode</TabsTrigger>
          <TabsTrigger value="cleaner">AI cleaner</TabsTrigger>
        </TabsList>

      <TabsContent value="dictation">
      <SettingsSectionHeader
        title="Speech-to-Text (STT)"
        description="Choose the engine used to transcribe your live microphone input"
      />
      <div className="settings-card provider-card-container">
        <RadioGroup
          className="provider-grid"
          id="stt-provider-grid"
          value={stt.preset}
          onValueChange={(preset) => void selectCard('stt', preset)}
          aria-label="Speech-to-text provider"
        >
          {STT_ORDER.map((preset) => (
            <RadioGroupItem key={preset} value={preset} asChild>
              <button
                type="button"
                className={`choice-surface provider-card${stt.preset === preset ? ' selected' : ''}`}
                data-provider={preset}
                id={STT_IDS[preset]}
              >
                <SttIcon preset={preset} />
                <span className="provider-name">{STT_NAMES[preset]}</span>
              </button>
            </RadioGroupItem>
          ))}
        </RadioGroup>
        <div className="provider-form-fields">
          <div id="stt-credentials-wrapper" className={`provider-credentials${isOffline ? ' hidden' : ''}`}>
            <Field label="API Endpoint" htmlFor="stt-base-url">
              <Input
                type="url"
                id="stt-base-url"
                placeholder="https://api.groq.com/openai"
                value={stt.baseUrl}
                onChange={(e) => setForm('stt', { ...forms.current.stt, baseUrl: e.target.value })}
              />
              {isCustomHttpsEndpoint(stt.baseUrl) && (
                <p className="field-warning">Custom endpoint: audio, transcripts, vocabulary and bearer credentials may be sent to this server.</p>
              )}
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
                <Button variant="secondary" id="stt-save-key-btn" className="provider-save-key" onClick={() => void onSaveKey('stt')}>Save Key</Button>
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
                    <SelectGroup>
                      {stt.models.map((m) => (
                        <SelectItem key={m} value={m}>{m}</SelectItem>
                      ))}
                    </SelectGroup>
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
            <div className="provider-test-row">
              <Button variant="secondary" id="stt-test-btn" onClick={() => void onTest('stt')}>Test Connection</Button>
              <div className="connection-status" id="stt-status" role="status">
                <div className={sttStatus.dot}></div>
                <span id="stt-status-text">{sttStatus.text}</span>
              </div>
            </div>
          </div>

          <div id="stt-offline-downloader" className={`offline-downloader${isOffline ? '' : ' hidden'}`}>
            <div className="offline-downloader-title">
              <DownloadIcon />
              Offline Model Manager
            </div>
            <div className="offline-downloader-sub">
              <span className="text-body-md offline-downloader-heading">Choose a model</span>
              <span className="text-muted offline-downloader-hint">Pick the option that fits how you dictate. You can change this any time.</span>
            </div>
            <RadioGroup
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
                  <div
                    id={cfg.cardId}
                    data-engine={cfg.engine}
                    className={`choice-surface offline-model-card${selected ? ' selected' : ''}`}
                  >
                    <RadioGroupItem value={cfg.engine} asChild>
                      <button
                        type="button"
                        className="offline-model-select"
                        aria-label={`Select ${cfg.title} model`}
                      >
                        <span className="offline-model-card-top">
                          <span className="offline-radio" aria-hidden="true" />
                          <span className="text-body-md offline-model-title">{cfg.title}</span>
                          {cfg.badge && <span className="offline-badge">{cfg.badge}</span>}
                        </span>
                        <span className="text-muted offline-model-desc">{cfg.desc}</span>
                        <span className="offline-metrics">
                          <span className="offline-metric" role="img" aria-label={`Speed ${cfg.speed} out of 5`}>
                            <span className="offline-metric-label">Speed</span>
                            <span className="offline-metric-segs">
                              {[1, 2, 3, 4, 5].map((i) => (
                                <i key={i} className={i <= cfg.speed ? 'on' : undefined} />
                              ))}
                            </span>
                          </span>
                          <span className="offline-metric" role="img" aria-label={`Accuracy ${cfg.accuracy} out of 5`}>
                            <span className="offline-metric-label">Accuracy</span>
                            <span className="offline-metric-segs">
                              {[1, 2, 3, 4, 5].map((i) => (
                                <i key={i} className={i <= cfg.accuracy ? 'on' : undefined} />
                              ))}
                            </span>
                          </span>
                        </span>
                      </button>
                    </RadioGroupItem>
                    <div className="offline-model-actions">
                      <Button
                        variant="default"
                        id={cfg.downloadBtnId}
                        className="offline-model-btn"
                        disabled={isInstalled || isDownloading}
                        onClick={() => void onDownload(cfg)}
                      >
                        {isInstalled ? 'Installed' : isDownloading ? 'Connecting…' : 'Download Model'}
                      </Button>
                      <Button
                        variant="destructive"
                        id={cfg.deleteBtnId}
                        className={`offline-model-btn${isInstalled ? '' : ' hidden'}`}
                        onClick={() => setDeleteTarget(cfg)}
                      >
                        Delete Model
                      </Button>
                    </div>
                  </div>
                );
              })}
            </RadioGroup>
            <div id="offline-progress-wrapper" className={`offline-progress${progressVisible ? '' : ' hidden'}`}>
              <div className="offline-progress-row">
                <span id="offline-progress-status" className="offline-progress-status">{progressStatus}</span>
                <span id="offline-progress-percentage" className="offline-progress-percentage">{progressPct.toFixed(0)}%</span>
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
              <div className="offline-progress-foot">
                <span id="offline-progress-bytes">{progressBytes}</span>
                <Button variant="ghost" size="xs" id="offline-cancel-btn" onClick={() => void onCancelDownload()}>Cancel</Button>
              </div>
            </div>
          </div>
        </div>
      </div>
      </TabsContent>

      <TabsContent value="agent">
        <LlmProviderSection
          kind="llm"
          form={llm}
          status={llmStatus}
          fetching={llmFetching}
          idPrefix="llm"
          title="Language Model (Agent Mode)"
          description="Choose the model that handles Agent Mode commands and text actions."
          onSelectCard={selectCard}
          onSetForm={setForm}
          onKeyInput={onKeyInput}
          onSaveKey={onSaveKey}
          onTest={onTest}
          onFetchModels={doFetchModels}
        />
      </TabsContent>

      <TabsContent value="cleaner">
        <LlmProviderSection
          kind="cleaner"
          form={cleaner}
          status={cleanerStatus}
          fetching={cleanerFetching}
          idPrefix="cleaner"
          title="AI Cleaner"
          description="Choose the model that cleans up your transcriptions after dictation."
          onSelectCard={selectCard}
          onSetForm={setForm}
          onKeyInput={onKeyInput}
          onSaveKey={onSaveKey}
          onTest={onTest}
          onFetchModels={doFetchModels}
        />
      </TabsContent>
      </Tabs>

      <div className="page-actions">
        <Button variant="default" id="save-providers-btn" onClick={() => void onSaveAll()}>Save Changes</Button>
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
