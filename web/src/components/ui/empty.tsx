import * as React from 'react';
import { Slot } from '@radix-ui/react-slot';
import { cn } from '@/lib/cn';

function Empty({ className, ...props }: React.ComponentProps<'div'>) {
  return (
    <div data-slot="empty" className={cn('empty-state', className)} {...props} />
  );
}

function EmptyHeader({ className, ...props }: React.ComponentProps<'div'>) {
  return (
    <div data-slot="empty-header" className={cn('empty-state-header', className)} {...props} />
  );
}

function EmptyMedia({
  className,
  variant = 'default',
  ...props
}: React.ComponentProps<'div'> & { variant?: 'default' | 'icon' }) {
  return (
    <Slot
      data-slot="empty-icon"
      data-variant={variant}
      className={cn('empty-state-icon', className)}
      {...props}
    />
  );
}

function EmptyTitle({ className, ...props }: React.ComponentProps<'div'>) {
  return (
    <div data-slot="empty-title" className={cn('empty-state-title', className)} {...props} />
  );
}

function EmptyDescription({ className, ...props }: React.ComponentProps<'div'>) {
  return (
    <div
      data-slot="empty-description"
      className={cn('empty-state-hint', className)}
      {...props}
    />
  );
}

function EmptyContent({ className, ...props }: React.ComponentProps<'div'>) {
  return (
    <div data-slot="empty-content" className={cn('empty-state-content', className)} {...props} />
  );
}

export { Empty, EmptyHeader, EmptyMedia, EmptyTitle, EmptyDescription, EmptyContent };
