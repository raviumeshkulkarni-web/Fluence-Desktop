import * as React from 'react';
import { cn } from '@/lib/cn';

export interface SettingsSectionHeaderProps {
  title: React.ReactNode;
  description?: React.ReactNode;
  children?: React.ReactNode;
  className?: string;
  id?: string;
}

/**
 * Section header rendered OUTSIDE cards, matching the official Codex UI.
 * High-contrast, clear hierarchy, with optional right-aligned actions.
 */
export function SettingsSectionHeader({
  title,
  description,
  children,
  className,
  id,
}: SettingsSectionHeaderProps) {
  return (
    <div className={cn('settings-section-header-outside', className)} id={id}>
      <div className="settings-section-header-text">
        <h2 className="settings-section-title">{title}</h2>
        {description && <p className="settings-section-desc">{description}</p>}
      </div>
      {children && <div className="settings-section-actions">{children}</div>}
    </div>
  );
}
