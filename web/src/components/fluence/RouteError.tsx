import { AlertCircle } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from '@/components/ui/empty';
import { cn } from '@/lib/cn';

export interface RouteErrorBannerProps {
  message: string;
  onRetry?: () => void;
  retryLabel?: string;
  className?: string;
}

export function RouteErrorBanner({
  message,
  onRetry,
  retryLabel = 'Try again',
  className,
}: RouteErrorBannerProps) {
  return (
    <div role="alert" className={cn('route-error-banner', className)}>
      <span>{message}</span>
      {onRetry ? (
        <Button onClick={onRetry} size="xs" variant="secondary">
          {retryLabel}
        </Button>
      ) : null}
    </div>
  );
}

export interface RouteErrorStateProps {
  title: string;
  description: string;
  onRetry: () => void;
  retryLabel?: string;
  id?: string;
  className?: string;
}

export function RouteErrorState({
  title,
  description,
  onRetry,
  retryLabel = 'Try again',
  id,
  className,
}: RouteErrorStateProps) {
  return (
    <Empty className={cn('agents-empty agents-error', className)} id={id} role="alert">
      <EmptyHeader>
        <EmptyMedia variant="icon">
          <AlertCircle aria-hidden="true" className="agents-empty-icon" strokeWidth={1.5} />
        </EmptyMedia>
        <EmptyTitle className="agents-empty-title">{title}</EmptyTitle>
        <EmptyDescription className="agents-empty-desc">{description}</EmptyDescription>
      </EmptyHeader>
      <EmptyContent>
        <Button onClick={onRetry} size="sm" variant="default">
          {retryLabel}
        </Button>
      </EmptyContent>
    </Empty>
  );
}
