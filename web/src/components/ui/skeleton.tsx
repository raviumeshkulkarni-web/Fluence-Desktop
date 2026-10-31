import * as React from 'react';
import { cn } from '@/lib/cn';

// shadcn-canonical Skeleton. Paint from ui.css (token pulse).
function Skeleton({
  className,
  ...props
}: React.HTMLAttributes<HTMLDivElement>) {
  return <div className={cn('skeleton', className)} {...props} />;
}

export { Skeleton };
