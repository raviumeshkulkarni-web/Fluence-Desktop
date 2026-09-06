import { useEffect, useRef, useState } from 'react';
import { getAppVersion } from '@/ipc/tauri';
import { updaterStore, useUpdater } from '@/ipc/updater';
import type { Route } from '@/App';

const NAV: { page: Route; label: string; icon: React.ReactNode }[] = [
  {
    page: 'dashboard',
    label: 'Dashboard',
    icon: (
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <path d="M3 3v16a2 2 0 0 0 2 2h16" />
        <path d="M7 14l4-4 4 3 5-6" />
      </svg>
    ),
  },
  {
    page: 'history',
    label: 'History',
    icon: (
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <circle cx="12" cy="12" r="10" />
        <polyline points="12 6 12 12 16 14" />
      </svg>
    ),
  },
  {
    page: 'general',
    label: 'General',
    icon: (
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <circle cx="12" cy="12" r="3" />
        <path d="M19.07 4.93a10 10 0 0 1 0 14.14M4.93 4.93a10 10 0 0 0 0 14.14" />
      </svg>
    ),
  },
  {
    page: 'providers',
    label: 'Providers',
    icon: (
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <rect x="2" y="3" width="20" height="14" rx="2" />
        <path d="M8 21h8M12 17v4" />
      </svg>
    ),
  },
  {
    page: 'dictionary',
    label: 'Dictionary',
    icon: (
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <path d="M4 19.5A2.5 2.5 0 0 1 6.5 17H20" />
        <path d="M6.5 2H20v20H6.5A2.5 2.5 0 0 1 4 19.5v-15A2.5 2.5 0 0 1 6.5 2z" />
      </svg>
    ),
  },
  {
    page: 'snippets',
    label: 'Snippets',
    icon: (
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <path d="M5 8V5h3M16 5h3v3M5 16v3h3M16 19h3v-3" />
        <path d="M12 8v8M9 11l3-3 3 3" />
      </svg>
    ),
  },
  {
    page: 'sync',
    label: 'Sync',
    icon: (
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <path d="M21 12a9 9 0 1 1-2.64-6.36" />
        <polyline points="21 3 21 9 15 9" />
      </svg>
    ),
  },
  {
    page: 'about',
    label: 'About',
    icon: (
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <circle cx="12" cy="12" r="10" />
        <line x1="12" y1="8" x2="12" y2="12" />
        <line x1="12" y1="16" x2="12.01" y2="16" />
      </svg>
    ),
  },
];

export function Sidebar({
  route,
  onNavigate,
}: {
  route: Route;
  onNavigate: (page: Route) => void;
}) {
  const [appVersion, setAppVersion] = useState('1.0.0');
  const updater = useUpdater();
  // Idle "✓ Up to date" transient: mirrors vanilla (shown 3s after a check
  // completes, only if the status line was visible).
  const [showUpToDate, setShowUpToDate] = useState(false);
  const prevState = useRef(updater.state);

  useEffect(() => {
    getAppVersion().then(setAppVersion).catch(() => undefined);
  }, []);

  useEffect(() => {
    if (prevState.current === 'checking' && updater.state === 'idle') {
      setShowUpToDate(true);
      const t = window.setTimeout(() => setShowUpToDate(false), 3000);
      prevState.current = updater.state;
      return () => window.clearTimeout(t);
    }
    prevState.current = updater.state;
    return undefined;
  }, [updater.state]);

  const widget = renderWidget(appVersion, showUpToDate);

  return (
    <nav className="sidebar" role="navigation" aria-label="Settings navigation">
      <div className="sidebar-logo" style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
        <svg width="32" height="32" viewBox="0 0 32 32" fill="none" xmlns="http://www.w3.org/2000/svg">
          <circle cx="16" cy="16" r="13" stroke="url(#logo-grad-sidebar)" strokeWidth="2.2" strokeDasharray="1.8 3" strokeLinecap="round" />
          <circle cx="16" cy="16" r="9" stroke="url(#logo-grad-sidebar)" strokeWidth="2.2" strokeDasharray="2 3" strokeLinecap="round" />
          <circle cx="16" cy="16" r="5" stroke="url(#logo-grad-sidebar)" strokeWidth="2.2" strokeDasharray="2 2" strokeLinecap="round" />
          <circle cx="16" cy="16" r="1.5" fill="url(#logo-grad-sidebar)" />
          <defs>
            <linearGradient id="logo-grad-sidebar" x1="0" y1="0" x2="32" y2="32" gradientUnits="userSpaceOnUse">
              <stop offset="0%" stopColor="#8B45D8" />
              <stop offset="50%" stopColor="#8B45D8" />
              <stop offset="100%" stopColor="#0BD6E3" />
            </linearGradient>
          </defs>
        </svg>
        <span className="logo-text" style={{ fontWeight: 600, fontSize: 22, letterSpacing: '-0.03em', display: 'inline-flex', alignItems: 'baseline' }}>
          flu<span style={{ color: 'var(--color-brand-cyan)' }}>ence</span>
          <span style={{ fontFamily: "'Allura',cursive", fontWeight: 400, fontSize: 20, marginLeft: 6, lineHeight: 1, color: 'var(--color-on-surface-variant)' }}>Transcribe</span>
        </span>
      </div>

      <span className="nav-section-label">Home</span>
      {NAV.slice(0, 2).map((item) => (
        <NavButton key={item.page} item={item} active={route === item.page} onNavigate={onNavigate} />
      ))}
      <span className="nav-section-label" style={{ marginTop: 8 }}>Configuration</span>
      {NAV.slice(2).map((item) => (
        <NavButton key={item.page} item={item} active={route === item.page} onNavigate={onNavigate} />
      ))}

      <div className="sidebar-footer">
        <div className="sidebar-update-widget" id="sidebar-update-widget">
          <div className={widget.labelClass} id="sidebar-version-label">{widget.label}</div>
          <div
            id="sidebar-update-status"
            className="sidebar-update-status"
            role="status"
            style={{ display: widget.status ? 'block' : 'none' }}
          >
            {widget.status ?? ''}
          </div>
          <div
            id="sidebar-update-progress-bar"
            className="sidebar-update-progress-bar"
            role="progressbar"
            aria-label="Update download progress"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={updater.progress}
            style={{ display: widget.showProgress ? 'block' : 'none' }}
          >
            <div
              id="sidebar-update-progress-fill"
              className="sidebar-update-progress-fill"
              style={{ width: `${updater.progress}%` }}
            />
          </div>
          <button
            id="sidebar-update-btn"
            className={widget.btnClass}
            disabled={widget.btnDisabled}
            onClick={widget.onAction}
          >
            <span id="sidebar-update-btn-text">{widget.btnText}</span>
          </button>
        </div>
      </div>
    </nav>
  );

  function renderWidget(version: string, upToDate: boolean) {
    const check = () => void updaterStore.checkForUpdates(true);
    const download = () => void updaterStore.startDownloadAndInstall();
    const restart = () => void updaterStore.restartApp();
    switch (updater.state) {
      case 'checking':
        return {
          label: `v${version}`, labelClass: 'sidebar-version-label',
          status: 'Checking for updates…', showProgress: false,
          btnText: 'Checking…', btnClass: 'sidebar-update-btn', btnDisabled: true, onAction: check,
        };
      case 'available':
        return {
          label: 'Update Available', labelClass: 'sidebar-version-label highlight-update',
          status: `v${updater.version} ready to download`, showProgress: false,
          btnText: `Download v${updater.version}`, btnClass: 'sidebar-update-btn btn-has-update',
          btnDisabled: false, onAction: download,
        };
      case 'downloading':
        return {
          label: 'Downloading Update', labelClass: 'sidebar-version-label highlight-update',
          status: `Downloading ${updater.progress}%`, showProgress: true,
          btnText: `Downloading ${updater.progress}%`, btnClass: 'sidebar-update-btn',
          btnDisabled: true, onAction: check,
        };
      case 'ready':
        return {
          label: 'Update Ready', labelClass: 'sidebar-version-label highlight-ready',
          status: 'Restart to apply update', showProgress: false,
          btnText: 'Restart Fluence', btnClass: 'sidebar-update-btn btn-ready',
          btnDisabled: false, onAction: restart,
        };
      case 'failed':
        return {
          label: `v${version}`, labelClass: 'sidebar-version-label',
          status: updater.error ?? 'Update check failed', showProgress: false,
          btnText: 'Try Again', btnClass: 'sidebar-update-btn',
          btnDisabled: false, onAction: check,
        };
      case 'idle':
      default:
        return {
          label: `v${version}`, labelClass: 'sidebar-version-label',
          status: upToDate ? '✓ Up to date' : null, showProgress: false,
          btnText: 'Check for Updates', btnClass: 'sidebar-update-btn',
          btnDisabled: false, onAction: check,
        };
    }
  }
}

function NavButton({
  item,
  active,
  onNavigate,
}: {
  item: (typeof NAV)[number];
  active: boolean;
  onNavigate: (page: Route) => void;
}) {
  return (
    <button
      type="button"
      className={active ? 'nav-item active' : 'nav-item'}
      data-page={item.page}
      id={`nav-${item.page}`}
      aria-current={active ? 'page' : undefined}
      onClick={() => onNavigate(item.page)}
    >
      {item.icon}
      {item.label}
    </button>
  );
}
