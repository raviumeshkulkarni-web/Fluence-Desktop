import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { Field, FieldGroup } from '@/components/ui/field';
import { Input, Textarea } from '@/components/ui/input';

export interface PromptEditorDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description: string;
  ariaLabel: string;
  nameId: string;
  nameValue: string;
  namePlaceholder: string;
  nameMaxLength?: number;
  onNameChange: (value: string) => void;
  instructionsId: string;
  instructionsValue: string;
  instructionsPlaceholder: string;
  instructionsMaxLength?: number;
  onInstructionsChange: (value: string) => void;
  saving: boolean;
  onSave: () => void;
}

export function PromptEditorDialog({
  open,
  onOpenChange,
  title,
  description,
  ariaLabel,
  nameId,
  nameValue,
  namePlaceholder,
  nameMaxLength = 30,
  onNameChange,
  instructionsId,
  instructionsValue,
  instructionsPlaceholder,
  instructionsMaxLength = 1000,
  onInstructionsChange,
  saving,
  onSave,
}: PromptEditorDialogProps) {
  return (
    <Dialog open={open} onOpenChange={(next) => {
        if (!next && !saving) onOpenChange(false);
      }}
    >
      <DialogContent aria-label={ariaLabel}>
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>{description}</DialogDescription>
        </DialogHeader>
        <FieldGroup>
          <Field label="Name" htmlFor={nameId} hint={`${nameValue.length} / ${nameMaxLength} characters`}>
            <Input
              id={nameId}
              maxLength={nameMaxLength}
              placeholder={namePlaceholder}
              value={nameValue}
              onChange={(e) => onNameChange(e.target.value)}
            />
          </Field>
          <Field label="Instructions" htmlFor={instructionsId} hint={`${instructionsValue.length} / ${instructionsMaxLength} characters`}>
            <Textarea
              id={instructionsId}
              maxLength={instructionsMaxLength}
              placeholder={instructionsPlaceholder}
              rows={4}
              value={instructionsValue}
              onChange={(e) => onInstructionsChange(e.target.value)}
            />
          </Field>
        </FieldGroup>
        <DialogFooter>
          <Button
            variant="secondary"
            onClick={() => onOpenChange(false)}
            disabled={saving}
          >
            Cancel
          </Button>
          <Button onClick={onSave} disabled={saving}>
            {saving ? 'Saving…' : 'Save'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
