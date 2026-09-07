import * as React from 'react';
import { cn } from '@/lib/cn';

// Controlled API mirrors the native elements 1:1. Base paint comes from the
// frozen global element selectors (`input:not([type=checkbox]...)`,
// `textarea`), so the kit adds no input chrome of its own — these exist for
// ref-forwarding consistency and as the single import point for routes.

const Input = React.forwardRef<HTMLInputElement, React.InputHTMLAttributes<HTMLInputElement>>(
  ({ className, type = 'text', ...props }, ref) => (
    <input ref={ref} type={type} className={cn(className)} {...props} />
  ),
);
Input.displayName = 'Input';

const Textarea = React.forwardRef<
  HTMLTextAreaElement,
  React.TextareaHTMLAttributes<HTMLTextAreaElement>
>(({ className, ...props }, ref) => (
  <textarea ref={ref} className={cn(className)} {...props} />
));
Textarea.displayName = 'Textarea';

export { Input, Textarea };
