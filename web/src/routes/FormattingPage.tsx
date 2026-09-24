import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { AppWindow, Check, Plus, RefreshCw } from 'lucide-react';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Field } from '@/components/ui/field';
import { Input, Textarea } from '@/components/ui/input';
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { Skeleton } from '@/components/ui/skeleton';
import { Switch } from '@/components/ui/switch';
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
}: {
  value: string;
  title: string;
  description: string;
  selected: boolean;
  onSetDefault: () => void;
  onEdit?: () => void;
  onDelete?: () => void;
}) {
  const hasActions = !selected || Boolean(onEdit || onDelete);

  return (
    <div
      className={`choice-surface selection-row style-card${selected ? ' selected' : ''}${hasActions ? ' has-actions' : ''}`}
    >
      <RadioGroupItem value={value} asChild>
        <button
          type="button"
          className="selection-row-main"
          aria-label={`Set ${title} as default style`}
        >
          <span className="selection-radio" aria-hidden="true" />
          <span className="selection-row-copy">
            <span className="selection-row-heading">
              <span className="selection-row-title">{title}</span>
              {selected && (
                <Badge variant="secondary" className="style-default-badge">
                  Default
                </Badge>
              )}
            </span>
            <span className="selection-row-description">{description}</span>
          </span>
        </button>
      </RadioGroupItem>
      {hasActions && (
        <div className="selection-row-actions">
          {!selected && (
            <Button variant="secondary" size="xs" onClick={onSetDefault}>
              Set as default
            </Button>
          )}
          {onEdit && (
            <Button variant="secondary" size="xs" onClick={onEdit}>
              Edit
            </Button>
          )}
          {onDelete && (
            <Button
              variant="danger"
              size="xs"
              className="agent-delete-btn destructive-action"
              onClick={onDelete}
            >
              Delete
            </Button>
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
  const [pickerStyle, setPickerStyle] = useState('proofread');

  const [showEditor, setShowEditor] = useState(false);
  const [editing, setEditing] = useState<CustomPromptStyle | null>(null);
  const [styleName, setStyleName] = useState('');
  const [styleHint, setStyleHint] = useState('');
  const [saving, setSaving] = useState(false);
  const [pendingDelete, setPendingDelete] = useState<CustomPromptStyle | null>(
    null,
  );
  const [pendingRemoveExe, setPendingRemoveExe] = useState<string | null>(null);

  const loadPrompts = useCallback(async () => {
    try {
      setPrompts(await getPrompts());
    } catch (err) {
      toast('Failed to load AI styles: ' + String(err), 'error');
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
    if (on) {
      const restore =
        lastNonNone && lastNonNone !== 'none' ? lastNonNone : 'default';
      setAiPolish(restore);
      if (getCachedSettings()) setSettingField('ai_polish_style', restore);
    } else {
      if (aiPolish !== 'none') setLastNonNone(aiPolish);
      setAiPolish('none');
      if (getCachedSettings()) setSettingField('ai_polish_style', 'none');
    }
  };

  const customs = prompts?.custom_styles ?? [];
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
    const b = builtins.find((x) => x.id === styleId);
    if (b) return b.title;
    const c = customs.find((x) => x.id === styleId);
    if (c) return c.name;
    if (styleId === 'default') return 'Default cleanup';
    return styleId;
  };

  // Global default style (mirrors the Agents page default agent): every app
  // without its own override uses this. The backend resolves "custom:<id>"
  // for the global style the same way it does for per-app overrides.
  const setDefaultStyle = (styleId: string) => {
    const next = styleId || 'default';
    setAiPolish(next);
    setLastNonNone(next);
    if (getCachedSettings()) setSettingField('ai_polish_style', next);
    toast(`${aiStyleLabel(next)} is now the default`, 'success');
  };

  const setExeAiStyle = async (exe: string, value: string) => {
    try {
      if (value === 'global') await clearPromptOverride(exe);
      else await setPromptOverride(exe, value);
      await loadPrompts();
    } catch (err) {
      toast(String(err).replace(/^Error:\s*/, ''), 'error');
    }
  };

  const openPicker = () => {
    setPickerQuery('');
    setPickerExe(null);
    setPickerStyle('proofread');
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
      toast(String(err).replace(/^Error:\s*/, ''), 'error');
    } finally {
      setSaving(false);
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
          ? `Style deleted, ${affected.length} app override(s) reset`
          : 'Style deleted',
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
            AI Post Processing
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
          AI Post Processing
        </h1>
        <p className="page-subtitle">
          Clean up transcripts automatically. Turn it on and every dictation
          is tidied while keeping your words. Create custom styles or
          assign apps for more control.
        </p>
      </div>

      <div className="settings-section settings-section--unboxed">
        <div className="setting-row">
          <div className="setting-info">
            <div className="setting-label">AI post processing</div>
            <div className="setting-desc">
              {enabled ? 'On' : 'Off, transcripts stay as spoken'}
            </div>
          </div>
          <div className="setting-control">
            <Field>
              <Switch
                id="ai-postprocessing-cb"
                aria-label="Enable AI post processing"
                checked={enabled}
                onCheckedChange={onToggleCleanup}
              />
            </Field>
          </div>
        </div>

        <p className="format-note">
          When on, every app uses {aiStyleLabel(enabled ? aiPolish : lastNonNone)}:
          filler words removed, grammar fixed, your words kept. Assign an
          app below to use a different style.
        </p>

        <div className="settings-section-header settings-section-header-actions">
          <h2>Custom styles</h2>
          <Button variant="primary" size="sm" onClick={openNewStyle}>
            <Plus data-icon="inline-start" size={14} aria-hidden="true" />
            New style
          </Button>
        </div>
        <RadioGroup
          className="style-choice-group"
          value={enabled ? aiPolish : ''}
          onValueChange={(styleId) => void setDefaultStyle(styleId)}
          aria-label="Default cleanup style"
        >
          <StyleChoiceCard
            value="default"
            title="Default cleanup"
            description="Filler words removed, grammar fixed, your words kept."
            selected={enabled && aiPolish === 'default'}
            onSetDefault={() => setDefaultStyle('default')}
          />
          {customs.map((style) => (
            <StyleChoiceCard
              key={style.id}
              value={style.id}
              title={style.name}
              description={style.hint}
              selected={enabled && aiPolish === style.id}
              onSetDefault={() => setDefaultStyle(style.id)}
              onEdit={() => openEditStyle(style)}
              onDelete={() => setPendingDelete(style)}
            />
          ))}
        </RadioGroup>
        {customs.length === 0 && (
          <p className="format-note">
            No custom styles yet. Create a reusable style for the way you want
            text cleaned up.
          </p>
        )}

        <div className="settings-section-header settings-section-header-actions is-spaced">
          <h2>App styles</h2>
          <Button
            variant="primary"
            size="sm"
            id="ai-style-add-app-btn"
            onClick={openPicker}
          >
            <Plus data-icon="inline-start" size={14} aria-hidden="true" />
            Add app
          </Button>
        </div>
        {aiRuleExes.length === 0 ? (
          <p className="format-note">
            No per-app styles. Every app uses the style above.
          </p>
        ) : (
          aiRuleExes.map((exe) => {
            const key = prompts
              ? (findKey(prompts.overrides, exe) ?? exe)
              : exe;
            const styleId = prompts?.overrides[key] ?? '';
            return (
              <div key={key.toLowerCase()} className="agent-row">
                <span className="agent-row-main agent-row-static">
                  <AppGlyph iconUrl={appIconOf(exe)} name={appNameOf(exe)} />
                  <span className="agent-row-text">
                    <span className="agent-row-title">{appNameOf(exe)}</span>
                  </span>
                  <Badge variant="secondary">{aiStyleLabel(styleId)}</Badge>
                </span>
                <span className="agent-row-actions">
                  <Button
                    variant="ghost"
                    size="sm"
                    className="agent-delete-btn destructive-action"
                    aria-label={`Remove style for ${appNameOf(exe)}`}
                    onClick={() => setPendingRemoveExe(key)}
                  >
                    Remove
                  </Button>
                </span>
              </div>
            );
          })
        )}
      </div>

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
            <Input
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
                  {builtins.map((b) => (
                    <SelectItem key={b.id} value={b.id}>
                      {b.title}
                    </SelectItem>
                  ))}
                  {customs.map((c) => (
                    <SelectItem key={c.id} value={c.id}>
                      {c.name}
                    </SelectItem>
                  ))}
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

      <Dialog
        open={showEditor}
        onOpenChange={(open) => {
          if (!open) setShowEditor(false);
        }}
      >
        <DialogContent aria-label={editing ? 'Edit style' : 'New style'}>
           <DialogHeader>
             <DialogTitle>{editing ? 'Edit style' : 'New style'}</DialogTitle>
             <DialogDescription>
               {editing
                 ? 'Update the instructions used when this style is applied.'
                 : 'Create reusable instructions for how Fluence should clean up text.'}
             </DialogDescription>
           </DialogHeader>
          <Field label="Name (max 30 characters)" htmlFor="style-name-input">
            <Input
              id="style-name-input"
              value={styleName}
              maxLength={30}
              placeholder="Translator"
              onChange={(e) => setStyleName(e.target.value)}
            />
          </Field>
          <Field
            label="Instructions (max 1000 characters)"
            htmlFor="style-hint-input"
          >
            <Textarea
              id="style-hint-input"
              value={styleHint}
              maxLength={1000}
              rows={4}
              placeholder="Always reply in Hindi, keep it short"
              onChange={(e) => setStyleHint(e.target.value)}
            />
          </Field>
          <DialogFooter>
            <Button
              variant="secondary"
              onClick={() => setShowEditor(false)}
              disabled={saving}
            >
              Cancel
            </Button>
            <Button onClick={() => void onSaveStyle()} disabled={saving}>
              {saving ? 'Saving…' : 'Save'}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

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

      <AlertDialog
        open={pendingDelete !== null}
        onOpenChange={(open) => {
          if (!open) setPendingDelete(null);
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete custom style</AlertDialogTitle>
            <AlertDialogDescription>
              {pendingDelete
                ? `This action cannot be undone. Delete "${pendingDelete.name}"? App styles using it reset to global.`
                : 'This action cannot be undone.'}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel asChild>
              <Button variant="secondary">Cancel</Button>
            </AlertDialogCancel>
            <AlertDialogAction asChild>
              <Button variant="danger" onClick={() => void onDeleteStyle()}>
                Delete
              </Button>
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </section>
  );
}
