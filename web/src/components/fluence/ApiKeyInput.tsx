import { useEffect, useState } from 'react';
import { Eye, EyeOff } from 'lucide-react';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Field } from '@/components/ui/field';
import { Input } from '@/components/ui/input';

export interface ApiKeyInputProps {
  id: string;
  label?: string;
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  hint?: string;
  error?: string;
  saved?: boolean;
  saveButtonId?: string;
  onSaveKey?: () => void;
}

export function ApiKeyInput({
  id,
  label = 'API Key',
  value,
  onChange,
  placeholder = 'sk-•••••••••••••••',
  hint,
  error,
  saved = false,
  saveButtonId,
  onSaveKey,
}: ApiKeyInputProps) {
  const [revealed, setRevealed] = useState(false);

  useEffect(() => {
    if (!value) setRevealed(false);
  }, [value]);

  const labelNode = saved ? (
    <span className="api-key-label-row">
      <span>{label}</span>
      <Badge className="api-key-saved-badge" variant="success">
        Key saved
      </Badge>
    </span>
  ) : (
    label
  );

  return (
    <Field label={labelNode} htmlFor={id} hint={hint} error={error}>
      <div className="input-with-btn">
        <div className="api-key-input-wrap">
          <Input
            id={id}
            className="api-key-input"
            type={revealed ? 'text' : 'password'}
            value={value}
            placeholder={saved && !value ? 'Saved securely. Enter a new key to replace it' : placeholder}
            autoComplete="off"
            onKeyDown={(e) => {
              if (e.key === 'Enter' && onSaveKey && value.trim()) {
                e.preventDefault();
                onSaveKey();
              }
            }}
            onChange={(e) => onChange(e.target.value)}
          />
          <button
            type="button"
            className="api-key-reveal-btn"
            aria-label={revealed ? 'Hide API key' : 'Show API key'}
            aria-pressed={revealed}
            onClick={() => setRevealed((prev) => !prev)}
          >
            {revealed ? (
              <EyeOff size={15} strokeWidth={2} aria-hidden="true" />
            ) : (
              <Eye size={15} strokeWidth={2} aria-hidden="true" />
            )}
          </button>
        </div>
        {onSaveKey ? (
          <Button
            id={saveButtonId}
            className="provider-save-key"
            variant={value.trim() ? 'primary' : 'secondary'}
            onClick={onSaveKey}
          >
            Save Key
          </Button>
        ) : null}
      </div>
    </Field>
  );
}
