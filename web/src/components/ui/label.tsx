import * as React from 'react';
import * as LabelPrimitive from '@radix-ui/react-label';
import { cn } from '@/lib/cn';

// Radix Label painted as a vanilla `.form-row label` (label-lg / 500 /
// on-surface-variant). Single source stays frozen in src/css.
const Label = React.forwardRef<
  React.ElementRef<typeof LabelPrimitive.Root>,
  React.ComponentPropsWithoutRef<typeof LabelPrimitive.Root>
>(({ className, ...props }, ref) => (
  <LabelPrimitive.Root ref={ref} className={cn('field-label', className)} {...props} />
));
Label.displayName = LabelPrimitive.Root.displayName;

export { Label };
