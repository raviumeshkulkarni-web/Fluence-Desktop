import * as React from 'react';
import * as SwitchPrimitive from '@radix-ui/react-switch';
import { cn } from '@/lib/cn';

// Radix Switch painted as the vanilla tactile hardware switch
// (global.css `.toggle-switch`, values cited in ui.css). Space toggles via
// Radix; the hidden-checkbox + sibling-selector mechanics are gone.
const Switch = React.forwardRef<
  React.ElementRef<typeof SwitchPrimitive.Root>,
  React.ComponentPropsWithoutRef<typeof SwitchPrimitive.Root>
>(({ className, ...props }, ref) => (
  <SwitchPrimitive.Root ref={ref} className={cn('switch', className)} {...props}>
    <span className="switch-track" aria-hidden="true" />
    <SwitchPrimitive.Thumb className="switch-thumb" />
  </SwitchPrimitive.Root>
));
Switch.displayName = SwitchPrimitive.Root.displayName;

export { Switch };
