import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { AppWindow, Check, RefreshCw } from 'lucide-react';
import { describeClaimOutcome } from '@/lib/claim';
import { t, tr } from '@/i18n';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card } from '@/components/ui/card';
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from '@/components/ui/empty';
import { Field } from '@/components/ui/field';
import { Input, Textarea } from '@/components/ui/input';
import { SearchField } from '@/components/fluence/SearchField';
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group';
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { Skeleton } from '@/components/ui/skeleton';
import { Switch } from '@/components/ui/switch';
import { SettingsSectionHeader } from '@/components/fluence/SettingsSection';
import {
  ConfirmDialog,
  Dialog,
  DialogContent,
  DialogFooter,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog';
import { toast } from '@/components/fluence/Toasts';
import {
  getCachedSettings,
  loadSettings,
  setSettingField,
} from '@/ipc/settings';
import {
  clearPromptOverride,
  claimLegacyPromptStyle,
  deletePromptStyle,
  listInstalledApps,
  getPrompts,
  savePromptStyle,
  setPromptOverride,
  type BuiltinPromptStyle,
  type CustomPromptStyle,
  type InstalledApp,
  type PromptsView,
} from '@/ipc/prompts';

function str(value: unknown, fallback: string): string {
  return typeof value === 'string' && value ? value : fallback;
}

function findKey(map: Record<string, string>, exe: string): string | undefined {
  const lower = exe.toLowerCase();
  return Object.keys(map).find((k) => k.toLowerCase() === lower);
}

// Real app icon with a generic glyph fallback (app not found locally).
function AppGlyph({
  iconUrl,
  name,
}: {
  iconUrl?: string;
  name: string;
}) {
  if (iconUrl) {
    return (
      <img
        src={iconUrl}
        alt={`${name} icon`}
        className="app-rule-icon"
        draggable={false}
      />
    );
  }
  return (
    <AppWindow
      className="app-rule-icon app-rule-icon-fallback"
      aria-hidden="true"
    />
  );
}

function StyleChoiceCard({
  value,
  title,
  description,
  selected,
  onSetDefault,
  onEdit,
  onDelete,
  claimable,
  claiming,
  onClaim,
}: {
  value: string;
  title: string;
  description: string;
  selected: boolean;
  onSetDefault: () => void;
  onEdit?: () => void;
  onDelete?: () => void;
  claimable: boolean;
  claiming: boolean;
  onClaim: () => void;
}) {
  const hasActions = claimable || !selected || Boolean(onEdit || onDelete);

  return (
    <div
      className={`choice-surface selection-row style-card${selected ? ' selected' : ''}${hasActions ? ' has-actions' : ''}${claimable ? ' style-card-unclaimed' : ''}`}
    >
      {claimable ? (
        // Not selectable: an unclaimed style cannot be made the default or bound
        // to an app, because those act on the account store where it does not
        // exist yet. The explicit claim is the only action offered.
        <div className="selection-row-main selection-row-inert">
          <span className="selection-radio" aria-hidden="true" />
          <span className="selection-row-copy">
            <span className="selection-row-heading">
              <span className="selection-row-title">{title}</span>
              <Badge variant="secondary" className="style-unclaimed-badge">
                {t('On this device')}
              </Badge>
            </span>
            <span className="selection-row-description">{description}</span>
            <span className="selection-row-description style-unclaimed-note">
              {t('Saved on this device before it was linked to an account.')}
            </span>
          </span>
        </div>
      ) : (
        <RadioGroupItem value={value} asChild>
          <button
            type="button"
            className="selection-row-main"
            aria-label={t('Set {name} as default style', { name: title })}
          >
            <span className="selection-radio" aria-hidden="true" />
            <span className="selection-row-copy">
              <span className="selection-row-heading">
                <span className="selection-row-title">{title}</span>
                {selected && (
                  <Badge variant="secondary" className="style-default-badge">
                    {t('Default')}
                  </Badge>
                )}
              </span>
              <span className="selection-row-description">{description}</span>
            </span>
          </button>
        </RadioGroupItem>
      )}
      {hasActions && (
        <div className="selection-row-actions">
          {claimable ? (
            <Button
              variant="secondary"
              size="xs"
              onClick={onClaim}
              disabled={claiming}
            >
              {claiming ? t('Adding…') : t('Add to my account')}
            </Button>
          ) : (
            <>
              {!selected && (
                <Button variant="secondary" size="xs" onClick={onSetDefault}>
                  {t('Set as default')}
                </Button>
              )}
              {onEdit && (
                <Button variant="secondary" size="xs" onClick={onEdit}>
                  {t('Edit')}
                </Button>
              )}
              {onDelete && (
                <Button
                  variant="destructive"
                  size="xs"
                  onClick={onDelete}
                >
                  {t('Delete')}
                </Button>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}

// AI Post Processing hub: one master toggle, one global style, custom
// styles, and per-app assignment from the apps installed on this PC.
// Nothing is pre-categorized: apps with no rule use the global style.
export function FormattingPage() {
  const [loading, setLoading] = useState(true);
  const [aiPolish, setAiPolish] = useState('none');
  const [lastNonNone, setLastNonNone] = useState('default');
  const [prompts, setPrompts] = useState<PromptsView | null>(null);
  const [installed, setInstalled] = useState<InstalledApp[]>([]);
  const [installedLoading, setInstalledLoading] = useState(false);
  const installedRequestRef = useRef<Promise<InstalledApp[]> | null>(null);
  const installedMountedRef = useRef(true);

  const [showPicker, setShowPicker] = useState(false);
  const [pickerQuery, setPickerQuery] = useState('');
  const [pickerExe, setPickerExe] = useState<string | null>(null);
  const [pickerStyle, setPickerStyle] = useState('default');

  const [showEditor, setShowEditor] = useState(false);
  const [editing, setEditing] = useState<CustomPromptStyle | null>(null);
  const [styleName, setStyleName] = useState('');
  const [styleHint, setStyleHint] = useState('');
  const [saving, setSaving] = useState(false);
  const [claimingStyleId, setClaimingStyleId] = useState<string | null>(null);
  const [pendingDelete, setPendingDelete] = useState<CustomPromptStyle | null>(
    null,
  );
  const [pendingRemoveExe, setPendingRemoveExe] = useState<string | null>(null);

  const loadPrompts = useCallback(async () => {
    try {
      setPrompts(await getPrompts());
    } catch (err) {
      toast(t('Failed to load AI styles: {error}', { error: String(err) }), 'error');
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        await loadSettings();
      } catch {
        /* controls keep safe defaults until settings arrive */
      }
      if (cancelled) return;
      const s = getCachedSettings();
      if (s) {
        const stored = str(s.ai_polish_style, 'none');
        setAiPolish(stored);
        if (stored !== 'none') setLastNonNone(stored);
      }
      await loadPrompts();
      if (!cancelled) setLoading(false);
    })();
    const onFocus = () => void loadPrompts();
    window.addEventListener('focus', onFocus);
    return () => {
      cancelled = true;
      window.removeEventListener('focus', onFocus);
    };
  }, [loadPrompts]);

  useEffect(() => {
    installedMountedRef.current = true;
    return () => {
      installedMountedRef.current = false;
      installedRequestRef.current = null;
    };
  }, []);

  const loadInstalledApps = useCallback(() => {
    if (installedRequestRef.current) return;
    setInstalledLoading(true);
    const request = listInstalledApps();
    installedRequestRef.current = request;
    void request
      .then((apps) => {
        if (installedMountedRef.current) setInstalled(apps);
      })
      .catch(() => {
        installedRequestRef.current = null;
        if (installedMountedRef.current) setInstalled([]);
      })
      .finally(() => {
        if (installedMountedRef.current) setInstalledLoading(false);
      });
  }, []);

  const enabled = aiPolish !== 'none';

  const onToggleCleanup = (on: boolean) => {
    try {
      if (on) {
        const restore =
          lastNonNone && lastNonNone !== 'none' ? lastNonNone : 'default';
        setAiPolish(restore);
        if (getCachedSettings()) setSettingField('ai_polish_style', restore);
        toast(t('AI post processing enabled'), 'success');
      } else {
        if (aiPolish !== 'none') setLastNonNone(aiPolish);
        setAiPolish('none');
        if (getCachedSettings()) setSettingField('ai_polish_style', 'none');
        toast(t('AI post processing disabled'), 'success');
      }
    } catch (err) {
      toast(t('Failed to update: {error}', { error: String(err) }), 'error');
    }
  };

  const customs = prompts?.custom_styles ?? [];
  // Unclaimed styles stay visible on the board with an explicit "Add to my
  // account", but must NOT be offered as a default or an app binding: the backend
  // rejects both ("Unknown style."), so listing them would only ever produce a
  // confusing error toast. `claimable` is a display hint from the same admission
  // gate the backend enforces.
  const assignableStyles = (list: CustomPromptStyle[]) =>
    list.filter((s) => !s.claimable);
  const builtins: BuiltinPromptStyle[] = prompts?.builtin_styles ?? [];

  const installedByExe = useMemo(() => {
    const map: Record<string, InstalledApp> = {};
    for (const app of installed) map[app.exe.toLowerCase()] = app;
    return map;
  }, [installed]);

  const appNameOf = (exe: string): string =>
    installedByExe[exe.toLowerCase()]?.name ?? exe;

  const appIconOf = (exe: string): string | undefined =>
    installedByExe[exe.toLowerCase()]?.icon_data_url;

  const aiRuleExes = prompts ? Object.keys(prompts.overrides) : [];

  const aiStyleLabel = (styleId: string): string => {
    if (styleId === 'default' || styleId === 'proofread') return 'Default cleanup';
    const b = builtins.find((x) => x.id === styleId);
    if (b) return b.title;
    const c = customs.find((x) => x.id === styleId);
    if (c) return c.name;
    return styleId;
  };

  // Global default style (mirrors the Agents page default agent): every app
  // without its own override uses this. The backend resolves "custom:<id>"
  // for the global style the same way it does for per-app overrides.
  // Normalize so that if not a recognized custom style, it defaults to 'default'.
  const rawDefault = (enabled ? aiPolish : lastNonNone) || 'default';
  const isCustomDefault = customs.some((c) => c.id === rawDefault);
  const effectiveDefault = isCustomDefault ? rawDefault : 'default';

  const setDefaultStyle = (styleId: string) => {
    const next = styleId || 'default';
    setLastNonNone(next);
    if (enabled) {
      setAiPolish(next);
      if (getCachedSettings()) setSettingField('ai_polish_style', next);
    }
    toast(t('{style} is now the default', { style: tr(aiStyleLabel(next)) }), 'success');
  };

  const setExeAiStyle = async (exe: string, value: string) => {
    try {
      if (value === 'global') {
        await clearPromptOverride(exe);
        toast(t('Removed style for {app}', { app: appNameOf(exe) }), 'success');
      } else {
        await setPromptOverride(exe, value);
        toast(t('Updated style for {app}', { app: appNameOf(exe) }), 'success');
      }
      await loadPrompts();
    } catch (err) {
      toast(String(err).replace(/^Error:\s*/, ''), 'error');
    }
  };

  const openPicker = () => {
    setPickerQuery('');
    setPickerExe(null);
    setPickerStyle('default');
    setShowPicker(true);
    loadInstalledApps();
  };

  const closePicker = () => {
    setShowPicker(false);
    setPickerQuery('');
    setPickerExe(null);
  };

  const pickerApps = (): InstalledApp[] => {
    const q = pickerQuery.trim().toLowerCase();
    if (!q) return installed;
    return installed.filter(
      (app) =>
        app.name.toLowerCase().includes(q) ||
        app.exe.toLowerCase().includes(q),
    );
  };

  const pickerCurrentNote = (exe: string): string => {
    if (!prompts) return t('No custom style');
    const key = findKey(prompts.overrides, exe);
    if (!key) return t('No custom style');
    return t('Currently: {style}', { style: tr(aiStyleLabel(prompts.overrides[key] ?? '')) });
  };

  const onAddRule = async () => {
    if (!pickerExe) {
      toast(t('Choose an app first'), 'error');
      return;
    }
    try {
      await setPromptOverride(pickerExe, pickerStyle);
      await loadPrompts();
      toast(t('{app} updated', { app: appNameOf(pickerExe) }), 'success');
      closePicker();
    } catch (err) {
      toast(String(err).replace(/^Error:\s*/, ''), 'error');
    }
  };

  const openNewStyle = () => {
    setEditing(null);
    setStyleName('');
    setStyleHint('');
    setShowEditor(true);
  };

  const openEditStyle = (style: CustomPromptStyle) => {
    setEditing(style);
    setStyleName(style.name);
    setStyleHint(style.hint);
    setShowEditor(true);
  };

  const onSaveStyle = async () => {
    if (!styleName.trim() || !styleHint.trim()) {
      toast(t('Please fill in both name and instructions'), 'error');
      return;
    }
    setSaving(true);
    try {
      await savePromptStyle(styleName.trim(), styleHint.trim(), editing?.id);
      await loadPrompts();
      setShowEditor(false);
      setEditing(null);
      toast(t('Style saved'), 'success');
    } catch (err) {
      toast(String(err).replace(/^Error:\s*/, ''), 'error');
    } finally {
      setSaving(false);
    }
  };

// Adopt one unclaimed pre-account style into the account signed in right now.
// Only the id crosses the boundary — the backend resolves the destination
// account — and the outcome is rendered through the shared pure helper so a
// refusal explains itself instead of reading as a silent no-op.
const onClaimStyle = async (style: CustomPromptStyle) => {
    setClaimingStyleId(style.id);
    try {
      const outcome = await claimLegacyPromptStyle(style.id);
      await loadPrompts();
      const message = describeClaimOutcome(outcome, 'style');
      toast(message.text, message.kind);
    } catch (err) {
      toast(String(err).replace(/^Error:\s*/, ''), 'error');
    } finally {
      setClaimingStyleId(null);
    }
  };

  const onDeleteStyle = async () => {
    if (!pendingDelete) return;
    const deletedId = pendingDelete.id;
    try {
      const affected = await deletePromptStyle(deletedId);
      // A deleted style can't stay the global default: fall back so the
      // page never points at an unknown style id.
      if (deletedId === aiPolish) {
        setAiPolish('default');
        if (getCachedSettings()) setSettingField('ai_polish_style', 'default');
      }
      if (deletedId === lastNonNone) setLastNonNone('default');
      await loadPrompts();
      toast(
        affected.length > 0
          ? t('Style deleted, {count} app override(s) reset', { count: affected.length })
          : t('Style deleted'),
        'success',
      );
    } catch (err) {
      toast(String(err).replace(/^Error:\s*/, ''), 'error');
    } finally {
      setPendingDelete(null);
    }
  };

  if (loading) {
    return (
      <section className="page active" id="page-formatting">
        <div className="page-header">
          <h1 className="page-title" tabIndex={-1}>
            {t('AI Post Processing')}
          </h1>
        </div>
        <Skeleton className="route-skeleton" />
      </section>
    );
  }

  return (
    <section className="page active" id="page-formatting">
      <div className="page-header">
        <h1 className="page-title" tabIndex={-1}>
          {t('AI Post Processing')}
        </h1>
        <p className="page-subtitle">
          {t('Clean up transcripts automatically while keeping your original words. Create custom styles or assign apps for more control.')}
        </p>
      </div>

      <SettingsSectionHeader
        title={t('AI Post Processing')}
        description={t('Automatically refine and clean up dictations')}
      />
      <Card className="settings-card">
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">{t('Enable AI Post Processing')}</div>
            <div className="setting-desc">
              {enabled
                ? t('On: transcripts are cleaned up automatically')
                : t('Off: transcripts stay exactly as spoken')}
            </div>
          </div>
          <div className="setting-control">
            <Field>
              <Switch
                id="ai-postprocessing-cb"
                aria-label={t('Enable AI post processing')}
                checked={enabled}
                onCheckedChange={onToggleCleanup}
              />
            </Field>
          </div>
        </div>
      </Card>

      <SettingsSectionHeader
        title={t('Cleanup Styles')}
        description={t('Choose a default cleanup style or create custom prompt instructions')}
      >
        <Button
          variant="default"
          size="sm"
          id="ai-style-new-btn"
          onClick={openNewStyle}
        >
          {t('Add Style')}
        </Button>
      </SettingsSectionHeader>
      <Card className="settings-card">
        <RadioGroup
          className="style-choice-group"
          value={effectiveDefault}
          onValueChange={(styleId) => void setDefaultStyle(styleId)}
          aria-label={t('Default cleanup style')}
        >
          <StyleChoiceCard
            value="default"
            title={t('Default cleanup')}
            description={t('Filler words removed, grammar fixed, your words kept.')}
selected={effectiveDefault === 'default'}
            onSetDefault={() => setDefaultStyle('default')}
            claimable={false}
            claiming={false}
            onClaim={() => {}}
          />
          {customs.map((style) => (
            <StyleChoiceCard
              key={style.id}
              value={style.id}
              title={style.name}
              description={style.hint}
              selected={effectiveDefault === style.id}
              claimable={Boolean(style.claimable)}
              claiming={claimingStyleId === style.id}
              onClaim={() => void onClaimStyle(style)}
              onSetDefault={() => setDefaultStyle(style.id)}
              onEdit={() => openEditStyle(style)}
              onDelete={() => setPendingDelete(style)}
            />
          ))}
        </RadioGroup>
      </Card>

      <SettingsSectionHeader
        title={t('App Styles')}
        description={t('Assign custom cleanup styles to specific applications')}
      >
        <Button
          variant="default"
          size="sm"
          id="ai-style-add-app-btn"
          onClick={openPicker}
        >
          {t('Add App')}
        </Button>
      </SettingsSectionHeader>
      <Card className="settings-card">
        {aiRuleExes.length === 0 ? (
          <Empty className="agents-empty">
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <AppWindow
                  className="agents-empty-icon"
                  strokeWidth={1.5}
                  aria-hidden="true"
                />
              </EmptyMedia>
              <EmptyTitle className="agents-empty-title">
                {t('No per-app styles yet')}
              </EmptyTitle>
              <EmptyDescription className="agents-empty-desc">
                {t('Every application will use the cleanup style selected above. Add an app to customize its style.')}
              </EmptyDescription>
            </EmptyHeader>
            <EmptyContent>
              <Button variant="secondary" size="sm" onClick={openPicker}>
                {t('Add your first app style')}
              </Button>
            </EmptyContent>
          </Empty>
        ) : (
          aiRuleExes.map((exe) => {
            const key = prompts
              ? (findKey(prompts.overrides, exe) ?? exe)
              : exe;
            const styleId = prompts?.overrides[key] ?? '';
            const normalizedStyleId =
              styleId === 'proofread' ? 'default' : styleId;
// Must use the SAME predicate as the Select's options below. If this stayed on
            // the unfiltered list, an override pointing at a style that is now
            // unclaimed would match neither the options nor the `!isKnown`
            // fallback item, and the Select would render a blank value instead of
            // the style's name. Keeping the predicate in one place is what stops
            // the two drifting apart again.
            const isKnown =
              normalizedStyleId === 'default' ||
              builtins.some((b) => b.id === normalizedStyleId) ||
              assignableStyles(customs).some((c) => c.id === normalizedStyleId);

            return (
              <div key={key.toLowerCase()} className="setting-row app-rule-row">
                <div className="setting-info app-rule-info">
                  <AppGlyph iconUrl={appIconOf(exe)} name={appNameOf(exe)} />
                  <span className="setting-label">{appNameOf(exe)}</span>
                </div>
                <div className="setting-control app-rule-control">
                  <Select
                    value={normalizedStyleId}
                    onValueChange={(val) => void setExeAiStyle(key, val)}
                  >
                    <SelectTrigger
                      className="select-sm app-rule-select"
                      aria-label={t('Style for {app}', { app: appNameOf(exe) })}
                    >
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectGroup>
                        <SelectItem value="default">{t('Default cleanup')}</SelectItem>
                        {builtins
                          .filter(
                            (b) => b.id !== 'proofread' && b.id !== 'default',
                          )
                          .map((b) => (
                            <SelectItem key={b.id} value={b.id}>
                              {tr(b.title)}
                            </SelectItem>
                          ))}
{/* Unclaimed styles are excluded: binding one to an app is
                            rejected backend-side ("Unknown style."), so offering it
                            would only ever produce a confusing error toast. They
                            remain visible on the board above with an explicit claim. */}
                        {assignableStyles(customs).map((c) => (
                          <SelectItem key={c.id} value={c.id}>
                            {c.name}
                          </SelectItem>
                        ))}
                        {!isKnown && styleId && (
                          <SelectItem value={styleId}>
                            {tr(aiStyleLabel(styleId))}
                          </SelectItem>
                        )}
                      </SelectGroup>
                    </SelectContent>
                  </Select>
                  <Button
                    variant="ghost"
                    size="sm"
                    className="agent-delete-btn destructive-action"
                    aria-label={t('Remove style for {app}', { app: appNameOf(exe) })}
                    onClick={() => setPendingRemoveExe(key)}
                  >
                    {t('Remove')}
                  </Button>
                </div>
              </div>
            );
          })
        )}
      </Card>

      <Dialog
        open={showPicker}
        onOpenChange={(open) => {
          if (!open) closePicker();
        }}
      >
        <DialogContent aria-label={t('Add app')}>
          <DialogHeader>
            <DialogTitle>{t('Add app style')}</DialogTitle>
            <DialogDescription>
              {t('Choose an installed app and the style Fluence should use there.')}
            </DialogDescription>
          </DialogHeader>
          <Field label={t('Search installed apps')} htmlFor="app-picker-search">
            <SearchField
              id="app-picker-search"
              value={pickerQuery}
              maxLength={60}
              placeholder={t('Search installed apps')}
              onChange={(e) => setPickerQuery(e.target.value)}
            />
          </Field>
          <RadioGroup
            className="app-picker-list"
            value={pickerExe ?? ''}
            onValueChange={(exe) => setPickerExe(exe)}
            aria-label={t('Choose app')}
          >
            {installedLoading && (
              <div className="format-empty-note" role="status">
                <RefreshCw
                  size={16}
                  strokeWidth={2}
                  className="animate-spin"
                  aria-hidden="true"
                />
                {t('Loading installed apps…')}
              </div>
            )}
            {!installedLoading && installed.length === 0 && (
              <div className="format-empty-note">
                {t('No installed apps found.')}
              </div>
            )}
            {!installedLoading && pickerApps().length === 0 && installed.length > 0 && (
              <div className="format-empty-note">{t('No apps match.')}</div>
            )}
            {pickerApps().map((app) => (
              <RadioGroupItem key={app.exe.toLowerCase()} value={app.exe} asChild>
                <button
                  type="button"
                  className="choice-surface app-pick-row"
                  aria-label={t('{app}: {note}', { app: app.name, note: pickerCurrentNote(app.exe) })}
                  onClick={(event) => {
                    if (pickerExe?.toLowerCase() === app.exe.toLowerCase()) {
                      event.preventDefault();
                      setPickerExe(null);
                    }
                  }}
                >
                  <AppGlyph iconUrl={app.icon_data_url} name={app.name} />
                  <span className="app-pick-name">{app.name}</span>
                  <span className="app-pick-note">
                    {pickerCurrentNote(app.exe)}
                  </span>
                  {pickerExe?.toLowerCase() === app.exe.toLowerCase() && (
                    <Check className="app-pick-check" aria-hidden="true" />
                  )}
                </button>
              </RadioGroupItem>
            ))}
          </RadioGroup>
          <Field label={t('Style')} htmlFor="app-picker-style">
            {prompts ? (
              <Select value={pickerStyle} onValueChange={setPickerStyle}>
                <SelectTrigger id="app-picker-style" className="select-md">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    <SelectItem value="default">{t('Default cleanup')}</SelectItem>
                    {builtins
                      .filter((b) => b.id !== 'proofread' && b.id !== 'default')
                      .map((b) => (
                        <SelectItem key={b.id} value={b.id}>
                          {tr(b.title)}
                        </SelectItem>
                      ))}
{assignableStyles(customs).map((c) => (
                      <SelectItem key={c.id} value={c.id}>
                        {c.name}
                      </SelectItem>
                    ))}
                  </SelectGroup>
                </SelectContent>
              </Select>
            ) : (
              <div className="format-empty-note">
                {t('Could not load styles. Close and try again.')}
              </div>
            )}
          </Field>
          <DialogFooter>
            <Button variant="secondary" onClick={closePicker}>
              {t('Cancel')}
            </Button>
            <Button
              onClick={() => void onAddRule()}
              disabled={!prompts || !pickerExe || installedLoading}
            >
              {t('Add')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog
        open={showEditor}
        onOpenChange={(open) => {
          if (!open) setShowEditor(false);
        }}
      >
        <DialogContent aria-label={editing ? t('Edit style') : t('New style')}>
           <DialogHeader>
             <DialogTitle>{editing ? t('Edit style') : t('New style')}</DialogTitle>
             <DialogDescription>
               {editing
                 ? t('Update the instructions used when this style is applied.')
                 : t('Create reusable instructions for how Fluence should clean up text.')}
             </DialogDescription>
           </DialogHeader>
          <Field label={t('Name (max 30 characters)')} htmlFor="style-name-input">
            <Input
              id="style-name-input"
              value={styleName}
              maxLength={30}
              placeholder={t('Translator')}
              onChange={(e) => setStyleName(e.target.value)}
            />
          </Field>
          <Field
            label={t('Instructions (max 1000 characters)')}
            htmlFor="style-hint-input"
          >
            <Textarea
              id="style-hint-input"
              value={styleHint}
              maxLength={1000}
              rows={4}
              placeholder={t('Always reply in Hindi, keep it short')}
              onChange={(e) => setStyleHint(e.target.value)}
            />
          </Field>
          <DialogFooter>
            <Button
              variant="secondary"
              onClick={() => setShowEditor(false)}
              disabled={saving}
            >
              {t('Cancel')}
            </Button>
            <Button onClick={() => void onSaveStyle()} disabled={saving}>
              {saving ? t('Saving…') : t('Save')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <ConfirmDialog
        open={pendingRemoveExe !== null}
        onOpenChange={(open) => {
          if (!open) setPendingRemoveExe(null);
        }}
        title={t('Remove app style')}
        body={
          pendingRemoveExe
            ? t('This action cannot be undone. Remove the style for "{app}"? It will use the default style.', { app: appNameOf(pendingRemoveExe) })
            : t('This action cannot be undone.')
        }
        confirmLabel={t('Remove')}
        danger
        onConfirm={() => {
          const target = pendingRemoveExe;
          if (target) void setExeAiStyle(target, 'global');
        }}
      />

      <AlertDialog
        open={pendingDelete !== null}
        onOpenChange={(open) => {
          if (!open) setPendingDelete(null);
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t('Delete custom style')}</AlertDialogTitle>
            <AlertDialogDescription>
              {pendingDelete
                ? t('This action cannot be undone. Delete "{name}"? App styles using it reset to global.', { name: pendingDelete.name })
                : t('This action cannot be undone.')}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel asChild>
              <Button variant="secondary">{t('Cancel')}</Button>
            </AlertDialogCancel>
            <AlertDialogAction asChild>
              <Button variant="destructive" onClick={() => void onDeleteStyle()}>
                {t('Delete')}
              </Button>
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </section>
  );
}
