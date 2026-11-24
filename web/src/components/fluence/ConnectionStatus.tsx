import { cn } from '@/lib/cn';

export type ConnectionDotKind = 'idle' | 'success' | 'error';

export interface ConnectionStatusProps {
  kind: ConnectionDotKind;
  text: string;
  id?: string;
  textId?: string;
  className?: string;
}

export function ConnectionStatus({
  kind,
  text,
  id,
  textId,
  className,
}: ConnectionStatusProps) {
  const dotClass =
    kind === 'success'
      ? 'dot dot-success'
      : kind === 'error'
        ? 'dot dot-error'
        : 'dot dot-idle';

  return (
    <div className={cn('connection-status', className)} id={id} role="status">
      <div className={dotClass} aria-hidden="true" />
      <span id={textId}>{text}</span>
    </div>
  );
}
