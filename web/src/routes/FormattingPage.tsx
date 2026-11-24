import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { AppWindow, Check, RefreshCw } from 'lucide-react';
import { describeClaimOutcome } from '@/lib/claim';
import { describeError } from '@/lib/errors';
import { Button } from '@/components/ui/button';
import { Card, CardContent } from '@/components/ui/card';
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from '@/components/ui/empty';
import { Field } from '@/components/ui/field';
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
import { ChoiceRow } from '@/components/fluence/ChoiceRow';
import { PageHeader } from '@/components/fluence/PageHeader';
import { PromptEditorDialog } from '@/components/fluence/PromptEditorDialog';
import { SettingRow } from '@/components/fluence/SettingRow';
import {
  ConfirmDialog,
  Dialog,
  DialogContent,
  DialogFooter,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
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

const LAST_NON_NONE_STYLE_KEY = 'fluence_ai_polish_last_style';

function readLastNonNoneStyle(): string {
  try {
    return window.localStorage.getItem(LAST_NON_NONE_STYLE_KEY) || 'default';
  } catch {
    return 'default';
  }
}

function rememberLastNonNoneStyle(style: string) {
  try {
    window.localStorage.setItem(LAST_NON_NONE_STYLE_KEY, style);
  } catch {
    // The current page state still works if storage is unavailable.
  }
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

// AI Cleanup hub: one master toggle, one global style, custom
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
      toast('Could not load AI styles. ' + describeError(err), 'error');
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
        if (stored !== 'none') {
          setLastNonNone(stored);
          rememberLastNonNoneStyle(stored);
        } else {
          setLastNonNone(readLastNonNoneStyle());
        }
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

  useEffect(() => {
    if (prompts && Object.keys(prompts.overrides).length > 0 && installed.length === 0) {
      loadInstalledApps();
    }
  }, [prompts, installed.length, loadInstalledApps]);

  const enabled = aiPolish !== 'none';

  const onToggleCleanup = (on: boolean) => {
    try {
      if (on) {
        const restore =
          lastNonNone && lastNonNone !== 'none' ? lastNonNone : 'default';
        rememberLastNonNoneStyle(restore);
        setAiPolish(restore);
        if (getCachedSettings()) setSettingField('ai_polish_style', restore);
        toast('AI cleanup enabled', 'success');
      } else {
        if (aiPolish !== 'none') {
          setLastNonNone(aiPolish);
          rememberLastNonNoneStyle(aiPolish);
        }
        setAiPolish('none');
        if (getCachedSettings()) setSettingField('ai_polish_style', 'none');
        toast('AI cleanup disabled', 'success');
      }
    } catch (err) {
      toast('Could not update the AI style. ' + describeError(err), 'error');
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
    rememberLastNonNoneStyle(next);
    setAiPolish(next);
    if (getCachedSettings()) setSettingField('ai_polish_style', next);
    toast(
      enabled
        ? `${aiStyleLabel(next)} is now the default`
        : `${aiStyleLabel(next)} set as default and AI Cleanup enabled`,
      'success',
    );
  };

  const setExeAiStyle = async (exe: string, value: string) => {
    try {
      if (value === 'global') {
        await clearPromptOverride(exe);
        toast(`Removed style for ${appNameOf(exe)}`, 'success');
      } else {
        await setPromptOverride(exe, value);
        toast(`Updated style for ${appNameOf(exe)}`, 'success');
      }
      await loadPrompts();
    } catch (err) {
      toast(describeError(err), 'error');
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
    if (!prompts) return 'No custom style';
    const key = findKey(prompts.overrides, exe);
    if (!key) return 'No custom style';
    return `Currently: ${aiStyleLabel(prompts.overrides[key] ?? '')}`;
  };

  const onAddRule = async () => {
    if (!pickerExe) {
      toast('Choose an app first', 'error');
      return;
    }
    try {
      await setPromptOverride(pickerExe, pickerStyle);
      await loadPrompts();
      toast(`${appNameOf(pickerExe)} updated`, 'success');
      closePicker();
    } catch (err) {
      toast(describeError(err), 'error');
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
      toast('Please fill in both name and instructions', 'error');
      return;
    }
    setSaving(true);
    try {
      await savePromptStyle(styleName.trim(), styleHint.trim(), editing?.id);
      await loadPrompts();
      setShowEditor(false);
      setEditing(null);
      toast('Style saved', 'success');
    } catch (err) {
      toast(describeError(err), 'error');
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
      toast(describeError(err), 'error');
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
      if (deletedId === lastNonNone) {
        setLastNonNone('default');
        rememberLastNonNoneStyle('default');
      }
      await loadPrompts();
      toast(
        affected.length > 0
          ? `Style deleted, ${affected.length} app override(s) reset`
          : 'Style deleted',
        'success',
      );
    } catch (err) {
      toast(describeError(err), 'error');
    } finally {
      setPendingDelete(null);
    }
  };

  if (loading) {
    return (
      <section className="page active" id="page-formatting">
        <PageHeader title="AI Cleanup" />
        <Skeleton className="route-skeleton" />
      </section>
    );
  }

  return (
    <section className="page active" id="page-formatting">
      <PageHeader
        title="AI Cleanup"
        subtitle="Clean up transcripts automatically while keeping your original words. Create custom styles or assign apps for more control."
      />

      <SettingsSectionHeader
        title="AI Cleanup"
        description="Automatically refine and clean up dictations"
      />
      <Card className="settings-card">
        <CardContent className="settings-card-content">
          <SettingRow
            label="Enable AI Cleanup"
            htmlFor="ai-postprocessing-cb"
            description={
              enabled
                ? 'On: transcripts are cleaned up automatically'
                : 'Off: transcripts stay exactly as spoken'
            }
          >
            <Switch
              id="ai-postprocessing-cb"
              aria-label="Enable AI cleanup"
              checked={enabled}
              onCheckedChange={onToggleCleanup}
            />
          </SettingRow>
        </CardContent>
      </Card>

      <SettingsSectionHeader
        title="Cleanup Styles"
        description="Choose a default cleanup style or create custom prompt instructions"
      >
        <Button
          variant="default"
          size="sm"
          id="ai-style-new-btn"
          onClick={openNewStyle}
        >
          Add Style
        </Button>
      </SettingsSectionHeader>
      <Card className="settings-card">
        <RadioGroup
          className="style-choice-group"
          value={effectiveDefault}
          onValueChange={(styleId) => void setDefaultStyle(styleId)}
          aria-label="Default cleanup style"
        >
          <ChoiceRow
            value="default"
            title="Default cleanup"
            description="Filler words removed, grammar fixed, your words kept."
            selected={effectiveDefault === 'default'}
            rowClassName="style-card"
            unclaimedClassName="style-card-unclaimed"
            defaultBadgeClassName="style-default-badge"
            unclaimedBadgeClassName="style-unclaimed-badge"
            unclaimedNoteClassName="style-unclaimed-note"
            selectAriaLabel="Set Default cleanup as default style"
          />
          {customs.map((style) => (
            <ChoiceRow
              key={style.id}
              value={style.id}
              title={style.name}
              description={style.hint}
              selected={effectiveDefault === style.id}
              rowClassName="style-card"
              unclaimedClassName="style-card-unclaimed"
              defaultBadgeClassName="style-default-badge"
              unclaimedBadgeClassName="style-unclaimed-badge"
              unclaimedNoteClassName="style-unclaimed-note"
              selectAriaLabel={`Set ${style.name} as default style`}
              claimable={Boolean(style.claimable)}
              claiming={claimingStyleId === style.id}
              onClaim={() => void onClaimStyle(style)}
              onEdit={() => openEditStyle(style)}
              onDelete={() => setPendingDelete(style)}
            />
          ))}
        </RadioGroup>
      </Card>

      <SettingsSectionHeader
        title="App Styles"
        description="Assign custom cleanup styles to specific applications"
      >
        <Button
          variant="default"
          size="sm"
          id="ai-style-add-app-btn"
          onClick={openPicker}
        >
          Add App
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
                No per-app styles yet
              </EmptyTitle>
              <EmptyDescription className="agents-empty-desc">
                Every application will use the cleanup style selected above.
                Add an app to customize its style.
              </EmptyDescription>
            </EmptyHeader>
            <EmptyContent>
              <Button variant="secondary" size="sm" onClick={openPicker}>
                Add your first app style
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
                      aria-label={`Style for ${appNameOf(exe)}`}
                    >
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectGroup>
                        <SelectItem value="default">Default cleanup</SelectItem>
                        {builtins
                          .filter(
                            (b) => b.id !== 'proofread' && b.id !== 'default',
                          )
                          .map((b) => (
                            <SelectItem key={b.id} value={b.id}>
                              {b.title}
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
                            {aiStyleLabel(styleId)}
                          </SelectItem>
                        )}
                      </SelectGroup>
                    </SelectContent>
                  </Select>
                  <Button
                    variant="ghost"
                    size="sm"
                    className="agent-delete-btn destructive-action"
                    aria-label={`Remove style for ${appNameOf(exe)}`}
                    onClick={() => setPendingRemoveExe(key)}
                  >
                    Remove
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
        <DialogContent aria-label="Add app">
          <DialogHeader>
            <DialogTitle>Add app style</DialogTitle>
            <DialogDescription>
              Choose an installed app and the style Fluence should use there.
            </DialogDescription>
          </DialogHeader>
          <Field label="Search installed apps" htmlFor="app-picker-search">
            <SearchField
              id="app-picker-search"
              value={pickerQuery}
              maxLength={60}
              placeholder="Search installed apps"
              onChange={(e) => setPickerQuery(e.target.value)}
            />
          </Field>
          <RadioGroup
            className="app-picker-list"
            value={pickerExe ?? ''}
            onValueChange={(exe) => setPickerExe(exe)}
            aria-label="Choose app"
          >
            {installedLoading && (
              <div className="format-empty-note" role="status">
                <RefreshCw
                  size={16}
                  strokeWidth={2}
                  className="animate-spin"
                  aria-hidden="true"
                />
                Loading installed apps…
              </div>
            )}
            {!installedLoading && installed.length === 0 && (
              <div className="format-empty-note">
                No installed apps found.
              </div>
            )}
            {!installedLoading && pickerApps().length === 0 && installed.length > 0 && (
              <div className="format-empty-note">No apps match.</div>
            )}
            {pickerApps().map((app) => (
              <RadioGroupItem key={app.exe.toLowerCase()} value={app.exe} asChild>
                <button
                  type="button"
                  className="choice-surface app-pick-row"
                  aria-label={`${app.name}: ${pickerCurrentNote(app.exe)}`}
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
          <Field label="Style" htmlFor="app-picker-style">
            {prompts ? (
              <Select value={pickerStyle} onValueChange={setPickerStyle}>
                <SelectTrigger id="app-picker-style" className="select-md">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    <SelectItem value="default">Default cleanup</SelectItem>
                    {builtins
                      .filter((b) => b.id !== 'proofread' && b.id !== 'default')
                      .map((b) => (
                        <SelectItem key={b.id} value={b.id}>
                          {b.title}
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
                Could not load styles. Close and try again.
              </div>
            )}
          </Field>
          <DialogFooter>
            <Button variant="secondary" onClick={closePicker}>
              Cancel
            </Button>
            <Button
              onClick={() => void onAddRule()}
              disabled={!prompts || !pickerExe || installedLoading}
            >
              Add
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <PromptEditorDialog
        open={showEditor}
        onOpenChange={setShowEditor}
        title={editing ? 'Edit style' : 'New style'}
        description={
          editing
            ? 'Update the instructions used when this style is applied.'
            : 'Create reusable instructions for how Fluence should clean up text.'
        }
        ariaLabel={editing ? 'Edit style' : 'New style'}
        nameId="style-name-input"
        nameValue={styleName}
        namePlaceholder="Translator"
        nameMaxLength={30}
        onNameChange={setStyleName}
        instructionsId="style-hint-input"
        instructionsValue={styleHint}
        instructionsPlaceholder="Always reply in Hindi, keep it short"
        instructionsMaxLength={1000}
        onInstructionsChange={setStyleHint}
        saving={saving}
        onSave={() => void onSaveStyle()}
      />

      <ConfirmDialog
        open={pendingRemoveExe !== null}
        onOpenChange={(open) => {
          if (!open) setPendingRemoveExe(null);
        }}
        title="Remove app style"
        body={
          pendingRemoveExe
            ? `This action cannot be undone. Remove the style for "${appNameOf(pendingRemoveExe)}"? It will use the default style.`
            : 'This action cannot be undone.'
        }
        confirmLabel="Remove"
        danger
        onConfirm={() => {
          const target = pendingRemoveExe;
          if (target) void setExeAiStyle(target, 'global');
        }}
      />

      <ConfirmDialog
        open={pendingDelete !== null}
        onOpenChange={(open) => {
          if (!open) setPendingDelete(null);
        }}
        title="Delete custom style"
        body={
          pendingDelete
            ? `This action cannot be undone. Delete "${pendingDelete.name}"? App styles using it reset to global.`
            : 'This action cannot be undone.'
        }
        confirmLabel="Delete"
        danger
        onConfirm={() => void onDeleteStyle()}
      />
    </section>
  );
}
