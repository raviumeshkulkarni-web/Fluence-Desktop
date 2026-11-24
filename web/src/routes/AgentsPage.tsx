import { useCallback, useEffect, useState } from 'react';
import { Zap } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Card, CardContent } from '@/components/ui/card';
import { SettingsSectionHeader } from '@/components/fluence/SettingsSection';
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from '@/components/ui/empty';
import { ConfirmDialog } from '@/components/ui/dialog';
import { RadioGroup } from '@/components/ui/radio-group';
import { Skeleton } from '@/components/ui/skeleton';
import { ChoiceRow } from '@/components/fluence/ChoiceRow';
import { PageHeader } from '@/components/fluence/PageHeader';
import { PromptEditorDialog } from '@/components/fluence/PromptEditorDialog';
import { RouteErrorState } from '@/components/fluence/RouteError';
import { toast } from '@/components/fluence/Toasts';
import {
  deleteAgent,
  getAgents,
  claimLegacyAgent,
  saveAgent,
  setDefaultAgent,
  type AgentsView,
  type CustomAgent,
} from '@/ipc/agents';
import { describeClaimOutcome } from '@/lib/claim';
import { describeError } from '@/lib/errors';

// Agents board. Same visual language as Android AgentsScreen: one card
// container, BUILT IN + CUSTOM AGENTS sections, plain rows with dividers.
// The built-in multipurpose agent is fixed. Selecting a row makes it the
// default for Agent Mode; the overlay picker can still override per turn.
export function AgentsPage() {
  const [view, setView] = useState<AgentsView | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [showEditor, setShowEditor] = useState(false);
  const [editing, setEditing] = useState<CustomAgent | null>(null);
  const [name, setName] = useState('');
  const [hint, setHint] = useState('');
  const [saving, setSaving] = useState(false);
  const [pendingDelete, setPendingDelete] = useState<CustomAgent | null>(null);
  const [claimingId, setClaimingId] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setView(await getAgents());
      setError(null);
    } catch (err) {
      const message = describeError(err);
      setError(message);
      toast('Failed to load agents: ' + message, 'error');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
    const onFocus = () => void load();
    window.addEventListener('focus', onFocus);
    return () => window.removeEventListener('focus', onFocus);
  }, [load]);

  const retryLoad = () => {
    setLoading(true);
    setError(null);
    void load();
  };

  const onSelectDefault = async (id: string) => {
    try {
      await setDefaultAgent(id);
      setView((prev) => (prev ? { ...prev, default_id: id } : prev));
      toast('Default agent updated', 'success');
    } catch (err) {
      toast(describeError(err), 'error');
    }
  };

  const openNew = () => {
    setEditing(null);
    setName('');
    setHint('');
    setShowEditor(true);
  };

  const openEdit = (agent: CustomAgent) => {
    setEditing(agent);
    setName(agent.name);
    setHint(agent.hint);
    setShowEditor(true);
  };

  const onSave = async () => {
    if (!name.trim() || !hint.trim()) {
      toast('Please fill in both name and instructions', 'error');
      return;
    }
    setSaving(true);
    try {
      await saveAgent(
        name.trim(),
        hint.trim(),
        editing?.id,
      );
      await load();
      setShowEditor(false);
      setEditing(null);
      toast('Agent saved', 'success');
    } catch (err) {
      toast(describeError(err), 'error');
    } finally {
      setSaving(false);
    }
  };

  const onDelete = async () => {
    if (!pendingDelete) return;
    try {
      await deleteAgent(pendingDelete.id);
      await load();
      toast('Agent deleted', 'success');
    } catch (err) {
      toast(describeError(err), 'error');
    } finally {
      setPendingDelete(null);
    }
  };

  // Adopt one unclaimed pre-account agent into the account signed in right now.
  //
  // The button sends only the id — the backend resolves the destination account
  // itself — and it claims exactly this record, never the whole unclaimed set.
  // The message comes from the backend's outcome via the shared pure helper, so
  // a refusal (deleted / duplicated / built-in) explains itself instead of
  // collapsing into a misleading "nothing to add".
  const onClaim = async (agent: CustomAgent) => {
    setClaimingId(agent.id);
    try {
      const outcome = await claimLegacyAgent(agent.id);
      await load();
      const message = describeClaimOutcome(outcome, 'agent');
      toast(message.text, message.kind);
    } catch (err) {
      toast(describeError(err), 'error');
    } finally {
      setClaimingId(null);
    }
  };

  if (loading) {
    return (
      <section className="page active" id="page-agents">
        <PageHeader title="Agents" />
        <Skeleton className="route-skeleton" />
      </section>
    );
  }

  if (error || !view) {
    return (
      <section className="page active" id="page-agents">
        <PageHeader
          title="Agents"
          subtitle="Saved agents are temporarily unavailable."
        />
        <RouteErrorState
          title="Unable to load agents"
          description={error || 'Try again to load your saved agents.'}
          onRetry={retryLoad}
        />
      </section>
    );
  }

  const customs = view.custom_agents;

  return (
    <section className="page active" id="page-agents">
      <PageHeader
        title="Agents"
        subtitle="Agents are saved instructions that shape how Agent Mode writes for you, e.g. keep my emails short and professional. They only change wording and can never take actions. Select one to make it your default."
      />

      <SettingsSectionHeader
        title="Built-in Agent"
        description="The default multipurpose agent that handles general dictation and formatting"
      />
      <Card
        className={`settings-card${view.default_id === view.builtin_id ? ' settings-card-selected' : ''}`}
      >
        <CardContent className="settings-card-content">
          <RadioGroup
            className="agent-choice-group"
            value={view.default_id}
            onValueChange={(id) => void onSelectDefault(id)}
            aria-label="Built-in agents"
          >
            <ChoiceRow
              value={view.builtin_id}
              title={view.builtin_name}
              description={view.builtin_description}
              selected={view.default_id === view.builtin_id}
              rowClassName="agent-card"
              unclaimedClassName="agent-card-unclaimed"
              defaultBadgeClassName="agent-default-badge"
              unclaimedBadgeClassName="agent-unclaimed-badge"
              unclaimedNoteClassName="agent-unclaimed-note"
              descriptionClassName="agent-row-desc"
              selectAriaLabel={`Set ${view.builtin_name} as default agent`}
            />
          </RadioGroup>
        </CardContent>
      </Card>

      <SettingsSectionHeader
        title="Custom Agents"
        description="Custom instructions for specific writing styles or workflows"
      >
        <Button
          variant="default"
          size="sm"
          id="agents-new-btn"
          onClick={openNew}
        >
          New Agent
        </Button>
      </SettingsSectionHeader>
      <Card className="settings-card">
        <CardContent className="settings-card-content">
          {customs.length === 0 ? (
            <Empty className="agents-empty">
              <EmptyHeader>
                <EmptyMedia variant="icon">
                  <Zap className="agents-empty-icon" strokeWidth={1.5} aria-hidden="true" />
                </EmptyMedia>
                <EmptyTitle className="agents-empty-title">No custom agents yet</EmptyTitle>
                <EmptyDescription className="agents-empty-desc">
                  Create one that writes the way you want, e.g. a translator
                  that always replies in Hindi
                </EmptyDescription>
              </EmptyHeader>
              <EmptyContent>
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={openNew}
                >
                  Create your first agent
                </Button>
              </EmptyContent>
            </Empty>
          ) : (
            <RadioGroup
              className="agent-choice-group"
              value={view.default_id}
              onValueChange={(id) => void onSelectDefault(id)}
              aria-label="Custom agents"
            >
              {customs.map((agent) => (
                <ChoiceRow
                  key={agent.id}
                  value={agent.id}
                  title={agent.name}
                  description={agent.hint}
                  selected={view.default_id === agent.id}
                  rowClassName="agent-card"
                  unclaimedClassName="agent-card-unclaimed"
                  defaultBadgeClassName="agent-default-badge"
                  unclaimedBadgeClassName="agent-unclaimed-badge"
                  unclaimedNoteClassName="agent-unclaimed-note"
                  descriptionClassName="agent-row-desc"
                  selectAriaLabel={`Set ${agent.name} as default agent`}
                  claimable={Boolean(agent.claimable)}
                  claiming={claimingId === agent.id}
                  onClaim={() => void onClaim(agent)}
                  onEdit={() => openEdit(agent)}
                  onDelete={() => setPendingDelete(agent)}
                />
              ))}
            </RadioGroup>
          )}
        </CardContent>
      </Card>

      <PromptEditorDialog
        open={showEditor}
        onOpenChange={setShowEditor}
        title={editing ? 'Edit agent' : 'New agent'}
        description={
          editing
            ? 'Update the instructions used when this agent responds.'
            : 'Create reusable instructions for how this agent should respond.'
        }
        ariaLabel={editing ? 'Edit agent' : 'New agent'}
        nameId="agent-name-input"
        nameValue={name}
        namePlaceholder="e.g. Translator"
        nameMaxLength={30}
        onNameChange={setName}
        instructionsId="agent-hint-input"
        instructionsValue={hint}
        instructionsPlaceholder="e.g. always reply in Hindi, keep it short"
        instructionsMaxLength={1000}
        onInstructionsChange={setHint}
        saving={saving}
        onSave={() => void onSave()}
      />

      <ConfirmDialog
        open={pendingDelete !== null}
        onOpenChange={(open) => {
          if (!open) setPendingDelete(null);
        }}
        title="Delete custom agent"
        body={
          pendingDelete
            ? `This action cannot be undone. Delete "${pendingDelete.name}"?`
            : 'This action cannot be undone.'
        }
        confirmLabel="Delete"
        danger
        onConfirm={() => void onDelete()}
      />
    </section>
  );
}
