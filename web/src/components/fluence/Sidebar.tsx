import { useEffect, useRef, useState, type ReactNode } from 'react';
import type { LucideIcon } from 'lucide-react';
import {
  BookOpen,
  Bot,
  Braces,
  CaseSensitive,
  CircleDot,
  Download,
  History,
  Info,
  LayoutDashboard,
  Monitor,
  Moon,
  PanelLeftClose,
  PanelLeftOpen,
  RefreshCw,
  RotateCcw,
  Server,
  Settings2,
  Sun,
} from 'lucide-react';
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from '@/components/ui/tooltip';
import { Button } from '@/components/ui/button';
import {
  Sidebar as SidebarPrimitive,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarSeparator,
  SidebarTrigger,
  useSidebar,
} from '@/components/ui/sidebar';
import { getAppVersion } from '@/ipc/tauri';
import { updaterStore, useUpdater } from '@/ipc/updater';
import type { ThemeChoice } from '@/lib/theme';
import type { Route } from '@/App';

// Sidebar theme control cycles Dark → Light → System (follows the OS live).
// The icon + label always show the active choice, never the target.
const THEME_CYCLE: Record<ThemeChoice, ThemeChoice> = {
  dark: 'light',
  light: 'auto',
  auto: 'dark',
};

const THEME_META: Record<ThemeChoice, { label: string; Icon: ReactNode }> = {
  dark: { label: 'Dark', Icon: <Moon className="sidebar-theme-icon" data-icon="inline-start" aria-hidden="true" /> },
  light: { label: 'Light', Icon: <Sun className="sidebar-theme-icon" data-icon="inline-start" aria-hidden="true" /> },
  auto: { label: 'System', Icon: <Monitor className="sidebar-theme-icon" data-icon="inline-start" aria-hidden="true" /> },
};

const NAV: { page: Route; label: string; icon: LucideIcon }[] = [
  { page: 'dashboard', label: 'Dashboard', icon: LayoutDashboard },
  { page: 'history', label: 'History', icon: History },
  { page: 'general', label: 'General', icon: Settings2 },
  { page: 'bubble', label: 'Floating Bubble', icon: CircleDot },
  { page: 'providers', label: 'Providers', icon: Server },
  { page: 'formatting', label: 'AI Post Processing', icon: CaseSensitive },
  { page: 'agents', label: 'Agents', icon: Bot },
  { page: 'dictionary', label: 'Dictionary', icon: BookOpen },
  { page: 'snippets', label: 'Snippets', icon: Braces },
  { page: 'sync', label: 'Sync', icon: RefreshCw },
  { page: 'about', label: 'About', icon: Info },
];

export function Sidebar({
  route,
  onNavigate,
  themeChoice,
  onCycleTheme,
}: {
  route: Route;
  onNavigate: (page: Route) => void;
  themeChoice: ThemeChoice;
  onCycleTheme: () => void;
}) {
  const { state: sidebarState } = useSidebar();
  const collapsed = sidebarState === 'collapsed';
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

  const themeButton = (
    <Button
      type="button"
      variant="ghost"
      className="sidebar-theme-btn"
      aria-label={collapsed
        ? `Theme: ${THEME_META[themeChoice].label}. Activate for ${THEME_META[THEME_CYCLE[themeChoice]].label}.`
        : `Theme: ${THEME_META[themeChoice].label} (Ctrl+Shift+L toggles dark and light)`}
      title={collapsed ? undefined : `Theme: ${THEME_META[themeChoice].label} — activate to cycle`}
      onClick={onCycleTheme}
    >
      {THEME_META[themeChoice].Icon}
      {!collapsed && <span className="sidebar-theme-label">{THEME_META[themeChoice].label}</span>}
    </Button>
  );

  const updateButton = (
    <Button
      id="sidebar-update-btn"
      type="button"
      variant="secondary"
      className={widget.btnClass}
      disabled={widget.btnDisabled}
      onClick={widget.onAction}
      aria-label={collapsed ? widget.btnText : undefined}
      title={collapsed ? undefined : widget.btnText}
    >
      <UpdateIcon className="sidebar-update-icon update-icon" data-icon="inline-start" aria-hidden="true" />
      <span id="sidebar-update-btn-text">{widget.btnText}</span>
    </Button>
  );

  return (
    <SidebarPrimitive
      role="navigation"
      aria-label="Settings navigation"
      collapsible="icon"
    >
      <SidebarHeader>
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
              <SidebarTrigger
                aria-label={collapsed ? 'Expand sidebar (Ctrl+B)' : 'Collapse sidebar (Ctrl+B)'}
                aria-expanded={!collapsed}
              >
                {collapsed ? <PanelLeftOpen data-icon="inline-start" aria-hidden="true" /> : <PanelLeftClose data-icon="inline-start" aria-hidden="true" />}
              </SidebarTrigger>
            </TooltipTrigger>
            <TooltipContent side="right">
              {collapsed ? 'Expand sidebar (Ctrl+B)' : 'Collapse sidebar (Ctrl+B)'}
            </TooltipContent>
          </Tooltip>
        </div>
        <SidebarSeparator className="sidebar-header-separator" />
      </SidebarHeader>

      <SidebarContent>
        <SidebarGroup>
          <SidebarGroupLabel className="nav-section-label">Home</SidebarGroupLabel>
          <SidebarMenu>
            {NAV.slice(0, 2).map((item) => (
              <NavButton key={item.page} item={item} active={route === item.page} onNavigate={onNavigate} />
            ))}
          </SidebarMenu>
        </SidebarGroup>
        <SidebarGroup className="sidebar-config-group">
          <SidebarGroupLabel className="nav-section-label">Configuration</SidebarGroupLabel>
          <SidebarMenu>
            {NAV.slice(2).map((item) => (
              <NavButton key={item.page} item={item} active={route === item.page} onNavigate={onNavigate} />
            ))}
          </SidebarMenu>
        </SidebarGroup>
      </SidebarContent>

      <SidebarSeparator className="sidebar-footer-separator" />
      <SidebarFooter>
        {collapsed ? (
          <Tooltip>
            <TooltipTrigger asChild>{themeButton}</TooltipTrigger>
            <TooltipContent side="right">{`Theme: ${THEME_META[themeChoice].label}`}</TooltipContent>
          </Tooltip>
        ) : (
          themeButton
        )}
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
              <TooltipTrigger asChild>{updateButton}</TooltipTrigger>
              <TooltipContent side="right">{widget.btnText}</TooltipContent>
            </Tooltip>
          ) : (
            updateButton
          )}
        </div>
      </SidebarFooter>
    </SidebarPrimitive>
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
  const Icon = item.icon;
  return (
    <SidebarMenuItem>
      <SidebarMenuButton
        isActive={active}
        tooltip={item.label}
        data-page={item.page}
        id={`nav-${item.page}`}
        aria-current={active ? 'page' : undefined}
        onClick={() => onNavigate(item.page)}
      >
        <Icon className="nav-icon" aria-hidden="true" />
        <span className="nav-item-label">{item.label}</span>
      </SidebarMenuButton>
    </SidebarMenuItem>
  );
}
