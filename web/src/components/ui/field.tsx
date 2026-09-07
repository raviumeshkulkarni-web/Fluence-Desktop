import * as React from 'react';
import { cn } from '@/lib/cn';
import { Label } from './label';

export interface FieldProps extends React.HTMLAttributes<HTMLDivElement> {
  label?: React.ReactNode;
  htmlFor?: string;
  hint?: React.ReactNode;
  error?: React.ReactNode;
}

// Labeled control group. Root reuses the frozen vanilla `.form-row`
// (column / 6px gap); label paints as `.form-row label`; error mirrors the
// wizard `.form-error` values (wizard.css is not loaded in the main window,
// so the declarations live on `.field-error` here). Hint has no vanilla
// ancestor — label-sm / ink-muted, tokens only.
function Field({ label, htmlFor, hint, error, className, children, ...props }: FieldProps) {
  return (
    <div className={cn('form-row', className)} {...props}>
      {label ? <Label htmlFor={htmlFor}>{label}</Label> : null}
      {children}
      {hint && !error ? <span className="field-hint">{hint}</span> : null}
      {error ? (
        <span className="field-error" role="alert">
          {error}
        </span>
      ) : null}
    </div>
  );
}

export { Field };
