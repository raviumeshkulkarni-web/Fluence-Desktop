import { useEffect, useRef, useState } from 'react';
import type { LucideIcon } from 'lucide-react';
import {
  BookOpen,
  Braces,
  Download,
  History,
  Info,
  LayoutDashboard,
  PanelLeftClose,
  PanelLeftOpen,
  RefreshCw,
  RotateCcw,
  Server,
  Settings2,
} from 'lucide-react';
import {
  Tooltip,
  TooltipContent,
  TooltipPortal,
  TooltipTrigger,
} from '@/components/ui/tooltip';
import { getAppVersion } from '@/ipc/tauri';
import { updaterStore, useUpdater } from '@/ipc/updater';
import type { Route } from '@/App';

const NAV: { page: Route; label: string; icon: LucideIcon }[] = [
  { page: 'dashboard', label: 'Dashboard', icon: LayoutDashboard },
  { page: 'history', label: 'History', icon: History },
  { page: 'general', label: 'General', icon: Settings2 },
  { page: 'providers', label: 'Providers', icon: Server },
  { page: 'dictionary', label: 'Dictionary', icon: BookOpen },
  { page: 'snippets', label: 'Snippets', icon: Braces },
  { page: 'sync', label: 'Sync', icon: RefreshCw },
  { page: 'about', label: 'About', icon: Info },
];

export function Sidebar({
  route,
  onNavigate,
  collapsed,
  onToggleCollapsed,
}: {
  route: Route;
  onNavigate: (page: Route) => void;
  collapsed: boolean;
  onToggleCollapsed: () => void;
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

  const UpdateIcon = updater.state === 'available'
    ? Download
    : updater.state === 'ready'
      ? RotateCcw
      : RefreshCw;

  return (
    <nav
      className={collapsed ? 'sidebar collapsed' : 'sidebar'}
      role="navigation"
      aria-label="Settings navigation"
      data-collapsed={collapsed ? 'true' : 'false'}
    >
      <div className="sidebar-logo">
        <div className="sidebar-brand">
        <svg className="sidebar-logo-mark" width="32" height="32" viewBox="0 0 32 32" fill="none" xmlns="http://www.w3.org/2000/svg">
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
        <span className="logo-text">
          flu<span style={{ color: 'var(--color-brand-cyan)' }}>ence</span>
          <span className="logo-tagline">Transcribe</span>
        </span>
        </div>
        <Tooltip>
          <TooltipTrigger asChild>
            <button
              type="button"
              className="sidebar-collapse-toggle"
              aria-label={collapsed ? 'Expand sidebar (Ctrl+B)' : 'Collapse sidebar (Ctrl+B)'}
              aria-expanded={!collapsed}
              onClick={onToggleCollapsed}
            >
              {collapsed ? <PanelLeftOpen aria-hidden="true" /> : <PanelLeftClose aria-hidden="true" />}
            </button>
          </TooltipTrigger>
          <TooltipPortal>
            <TooltipContent side="right">
              {collapsed ? 'Expand sidebar (Ctrl+B)' : 'Collapse sidebar (Ctrl+B)'}
            </TooltipContent>
          </TooltipPortal>
        </Tooltip>
      </div>

      <span className="nav-section-label">Home</span>
      {NAV.slice(0, 2).map((item) => (
        <NavButton key={item.page} item={item} active={route === item.page} collapsed={collapsed} onNavigate={onNavigate} />
      ))}
      <span className="nav-section-label" style={{ marginTop: 8 }}>Configuration</span>
      {NAV.slice(2).map((item) => (
        <NavButton key={item.page} item={item} active={route === item.page} collapsed={collapsed} onNavigate={onNavigate} />
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
          {collapsed ? (
            <Tooltip>
              <TooltipTrigger asChild>
                <button
                  id="sidebar-update-btn"
                  className={widget.btnClass}
                  disabled={widget.btnDisabled}
                  onClick={widget.onAction}
                  aria-label={widget.btnText}
                >
                  <UpdateIcon className="sidebar-update-icon update-icon" aria-hidden="true" />
                  <span id="sidebar-update-btn-text">{widget.btnText}</span>
                </button>
              </TooltipTrigger>
              <TooltipPortal>
                <TooltipContent side="right">{widget.btnText}</TooltipContent>
              </TooltipPortal>
            </Tooltip>
          ) : (
            <button
              id="sidebar-update-btn"
              className={widget.btnClass}
              disabled={widget.btnDisabled}
              onClick={widget.onAction}
              title={widget.btnText}
            >
              <UpdateIcon className="sidebar-update-icon update-icon" aria-hidden="true" />
              <span id="sidebar-update-btn-text">{widget.btnText}</span>
            </button>
          )}
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
  collapsed,
  onNavigate,
}: {
  item: (typeof NAV)[number];
  active: boolean;
  collapsed: boolean;
  onNavigate: (page: Route) => void;
}) {
  const Icon = item.icon;
  const button = (
    <button
      type="button"
      className={active ? 'nav-item active' : 'nav-item'}
      data-page={item.page}
      id={`nav-${item.page}`}
      aria-current={active ? 'page' : undefined}
      aria-label={collapsed ? item.label : undefined}
      onClick={() => onNavigate(item.page)}
    >
      <Icon className="nav-icon" aria-hidden="true" />
      <span className="nav-item-label">{item.label}</span>
    </button>
  );

  if (collapsed) {
    return (
      <Tooltip>
        <TooltipTrigger asChild>{button}</TooltipTrigger>
        <TooltipPortal>
          <TooltipContent side="right">{item.label}</TooltipContent>
        </TooltipPortal>
      </Tooltip>
    );
  }

  return button;
}
