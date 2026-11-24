import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { RadioGroupItem } from '@/components/ui/radio-group';
import { cn } from '@/lib/cn';

export interface ChoiceRowProps {
  value: string;
  title: string;
  description: string;
  selected: boolean;
  selectAriaLabel: string;
  rowClassName?: string;
  unclaimedClassName?: string;
  defaultBadgeClassName?: string;
  unclaimedBadgeClassName?: string;
  unclaimedNoteClassName?: string;
  descriptionClassName?: string;
  claimable?: boolean;
  claiming?: boolean;
  onClaim?: () => void;
  onEdit?: () => void;
  onDelete?: () => void;
}

export function ChoiceRow({
  value,
  title,
  description,
  selected,
  selectAriaLabel,
  rowClassName,
  unclaimedClassName,
  defaultBadgeClassName,
  unclaimedBadgeClassName,
  unclaimedNoteClassName,
  descriptionClassName,
  claimable = false,
  claiming = false,
  onClaim,
  onEdit,
  onDelete,
}: ChoiceRowProps) {
  const hasActions = claimable || Boolean(onEdit || onDelete);

  return (
    <div
      className={cn(
        'choice-surface selection-row',
        rowClassName,
        selected && 'selected',
        hasActions && 'has-actions',
        claimable && unclaimedClassName,
      )}
    >
      {claimable ? (
        <div className="selection-row-main selection-row-inert">
          <span className="selection-radio" aria-hidden="true" />
          <span className="selection-row-copy">
            <span className="selection-row-heading">
              <span className="selection-row-title">{title}</span>
              <Badge className={unclaimedBadgeClassName} variant="secondary">
                On this device
              </Badge>
            </span>
            <span className={cn('selection-row-description', descriptionClassName)}>
              {description}
            </span>
            <span className={cn('selection-row-description', unclaimedNoteClassName)}>
              Saved on this device before it was linked to an account.
            </span>
          </span>
        </div>
      ) : (
        <RadioGroupItem asChild value={value}>
          <button
            type="button"
            className="selection-row-main"
            aria-label={selectAriaLabel}
          >
            <span className="selection-radio" aria-hidden="true" />
            <span className="selection-row-copy">
              <span className="selection-row-heading">
                <span className="selection-row-title">{title}</span>
                {selected ? (
                  <Badge className={defaultBadgeClassName} variant="secondary">
                    Default
                  </Badge>
                ) : null}
              </span>
              <span className={cn('selection-row-description', descriptionClassName)}>
                {description}
              </span>
            </span>
          </button>
        </RadioGroupItem>
      )}
      {hasActions ? (
        <div className="selection-row-actions">
          {claimable ? (
            <Button disabled={claiming} onClick={onClaim} size="xs" variant="secondary">
              {claiming ? 'Adding…' : 'Add to my account'}
            </Button>
          ) : (
            <>
              {onEdit ? (
                <Button onClick={onEdit} size="xs" variant="secondary">
                  Edit
                </Button>
              ) : null}
              {onDelete ? (
                <Button onClick={onDelete} size="xs" variant="destructive">
                  Delete
                </Button>
              ) : null}
            </>
          )}
        </div>
      ) : null}
    </div>
  );
}
