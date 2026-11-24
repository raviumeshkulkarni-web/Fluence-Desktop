import type * as React from 'react';
import { cn } from '@/lib/cn';

export interface SettingRowProps {
  label: React.ReactNode;
  description?: React.ReactNode;
  htmlFor?: string;
  labelId?: string;
  descriptionId?: string;
  descriptionRole?: React.AriaRole;
  descriptionAriaLive?: 'off' | 'polite' | 'assertive';
  extraInfo?: React.ReactNode;
  className?: string;
  children: React.ReactNode;
}

export function SettingRow({
  label,
  description,
  htmlFor,
  labelId,
  descriptionId,
  descriptionRole,
  descriptionAriaLive,
  extraInfo,
  className,
  children,
}: SettingRowProps) {
  return (
    <div className={cn('setting-row', className)}>
      <div className="setting-info">
        {htmlFor ? (
          <label className="setting-label setting-label-clickable" htmlFor={htmlFor} id={labelId}>
            {label}
          </label>
        ) : (
          <div className="setting-label" id={labelId}>
            {label}
          </div>
        )}
        {description ? (
          <div
            className="setting-desc"
            id={descriptionId}
            role={descriptionRole}
            aria-live={descriptionAriaLive}
          >
            {description}
          </div>
        ) : null}
        {extraInfo}
      </div>
      <div className="setting-control">{children}</div>
    </div>
  );
}
