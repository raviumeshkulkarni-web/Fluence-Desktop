import * as React from 'react';
import { cn } from '@/lib/cn';

export interface KbdProps extends React.HTMLAttributes<HTMLElement> {}

const Kbd = React.forwardRef<HTMLElement, KbdProps>(
  ({ className, ...props }, ref) => (
    <kbd
      ref={ref}
      className={cn('kbd-keycap', className)}
      {...props}
    />
  ),
);
Kbd.displayName = 'Kbd';

export { Kbd };
