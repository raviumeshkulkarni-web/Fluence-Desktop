import * as React from 'react';
import { cn } from '@/lib/cn';
import { Label } from './label';

export interface FieldProps extends React.HTMLAttributes<HTMLDivElement> {
  label?: React.ReactNode;
  htmlFor?: string;
  hint?: React.ReactNode;
  error?: React.ReactNode;
  orientation?: 'vertical' | 'horizontal';
}

function FieldGroup({ className, ...props }: React.ComponentProps<'div'>) {
  return <div data-slot="field-group" className={cn('field-group', className)} {...props} />;
}

function FieldContent({ className, ...props }: React.ComponentProps<'div'>) {
  return <div data-slot="field-content" className={cn('field-content', className)} {...props} />;
}

function FieldLabel({ className, ...props }: React.ComponentProps<typeof Label>) {
  return <Label data-slot="field-label" className={cn('field-label', className)} {...props} />;
}

function FieldDescription({ className, ...props }: React.ComponentProps<'p'>) {
  return <p data-slot="field-description" className={cn('field-hint', className)} {...props} />;
}

function FieldError({ className, ...props }: React.ComponentProps<'p'>) {
  return <p data-slot="field-error" className={cn('field-error', className)} {...props} />;
}

function Field({
  label,
  htmlFor,
  hint,
  error,
  orientation = 'vertical',
  className,
  children,
  ...props
}: FieldProps) {
  return (
    <div
      role="group"
      data-slot="field"
      data-orientation={orientation}
      className={cn('form-row', className)}
      {...props}
    >
      {label ? <FieldLabel htmlFor={htmlFor}>{label}</FieldLabel> : null}
      {children}
      {hint && !error ? <FieldDescription>{hint}</FieldDescription> : null}
      {error ? <FieldError role="alert">{error}</FieldError> : null}
    </div>
  );
}

export { Field, FieldGroup, FieldContent, FieldLabel, FieldDescription, FieldError };
