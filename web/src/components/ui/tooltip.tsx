import * as React from 'react';
import * as TooltipPrimitive from '@radix-ui/react-tooltip';
import { cn } from '@/lib/cn';

// Single-sourced delay for every tooltip in the app. Content is portaled by
// default via Radix's portal mechanism to avoid clipping in overflow containers
// (such as the collapsed icon-rail sidebar) while preserving z-index and styles.
const TooltipProvider = TooltipPrimitive.Provider;
const Tooltip = TooltipPrimitive.Root;
const TooltipTrigger = TooltipPrimitive.Trigger;

const TOOLTIP_DELAY_MS = 400;

const TooltipContent = React.forwardRef<
  React.ElementRef<typeof TooltipPrimitive.Content>,
  React.ComponentPropsWithoutRef<typeof TooltipPrimitive.Content>
>(({ className, sideOffset = 6, ...props }, ref) => (
  <TooltipPrimitive.Portal>
    <TooltipPrimitive.Content
      ref={ref}
      sideOffset={sideOffset}
      className={cn('tooltip-content', className)}
      {...props}
    />
  </TooltipPrimitive.Portal>
));
TooltipContent.displayName = TooltipPrimitive.Content.displayName;

const TooltipPortal = TooltipPrimitive.Portal;

export { TooltipProvider, Tooltip, TooltipTrigger, TooltipPortal, TooltipContent, TOOLTIP_DELAY_MS };
