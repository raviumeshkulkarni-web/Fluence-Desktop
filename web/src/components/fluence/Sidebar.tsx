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
import { FluenceSymbol, FluenceWordmark } from './FluenceLogo';
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
import { t, tr, type TranslationKey } from '@/i18n';

// Sidebar theme control cycles Dark → Light → System (follows the OS live).
// The icon + label always show the active choice, never the target.
const THEME_CYCLE: Record<ThemeChoice, ThemeChoice> = {
  dark: 'light',
  light: 'auto',
  auto: 'dark',
};

const THEME_META: Record<ThemeChoice, { label: TranslationKey; Icon: ReactNode }> = {
  dark: { label: 'Dark', Icon: <Moon className="sidebar-theme-icon" data-icon="inline-start" aria-hidden="true" /> },
  light: { label: 'Light', Icon: <Sun className="sidebar-theme-icon" data-icon="inline-start" aria-hidden="true" /> },
  auto: { label: 'System', Icon: <Monitor className="sidebar-theme-icon" data-icon="inline-start" aria-hidden="true" /> },
};

const NAV: { page: Route; label: TranslationKey; icon: LucideIcon }[] = [
  { page: 'dashboard', label: 'Dashboard', icon: LayoutDashboard },
  { page: 'history', label: 'History', icon: History },
  { page: 'general', label: 'General', icon: Settings2 },
  { page: 'bubble', label: 'Floating Bubble', icon: CircleDot },
  { page: 'providers', label: 'Providers', icon: Server },
  { page: 'formatting', label: 'AI Cleanup', icon: CaseSensitive },
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
  onDownloadUpdate,
}: {
  route: Route;
  onNavigate: (page: Route) => void;
  themeChoice: ThemeChoice;
  onCycleTheme: () => void;
  onDownloadUpdate: () => void;
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
        ? t('Theme: {theme}. Activate for {next}.', {
            theme: t(THEME_META[themeChoice].label),
            next: t(THEME_META[THEME_CYCLE[themeChoice]].label),
          })
        : t('Theme: {theme} (Ctrl+Shift+L toggles dark and light)', {
            theme: t(THEME_META[themeChoice].label),
          })}
      title={collapsed ? undefined : t('Theme: {theme}. Click to cycle', { theme: t(THEME_META[themeChoice].label) })}
      onClick={onCycleTheme}
    >
      {THEME_META[themeChoice].Icon}
      {!collapsed && <span className="sidebar-theme-label">{t(THEME_META[themeChoice].label)}</span>}
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
      aria-label={t('Settings navigation')}
      collapsible="icon"
    >
      <SidebarHeader>
        <div className="sidebar-logo">
          <div className="sidebar-brand">
            <FluenceSymbol
              size={32}
              gradientId="logo-grad-sidebar"
              className="sidebar-logo-mark"
            />
            <FluenceWordmark />
          </div>
          <Tooltip>
            <TooltipTrigger asChild>
              <SidebarTrigger
                aria-label={collapsed ? t('Expand sidebar (Ctrl+B)') : t('Collapse sidebar (Ctrl+B)')}
                aria-expanded={!collapsed}
              >
                {collapsed ? <PanelLeftOpen data-icon="inline-start" aria-hidden="true" /> : <PanelLeftClose data-icon="inline-start" aria-hidden="true" />}
              </SidebarTrigger>
            </TooltipTrigger>
            <TooltipContent side="right">
              {collapsed ? t('Expand sidebar (Ctrl+B)') : t('Collapse sidebar (Ctrl+B)')}
            </TooltipContent>
          </Tooltip>
        </div>
        <SidebarSeparator className="sidebar-header-separator" />
      </SidebarHeader>

      <SidebarContent>
        <SidebarGroup>
          <SidebarGroupLabel className="nav-section-label">{t('Home')}</SidebarGroupLabel>
          <SidebarMenu>
            {NAV.slice(0, 2).map((item) => (
              <NavButton key={item.page} item={item} active={route === item.page} onNavigate={onNavigate} />
            ))}
          </SidebarMenu>
        </SidebarGroup>
        <SidebarGroup className="sidebar-config-group">
          <SidebarGroupLabel className="nav-section-label">{t('Configuration')}</SidebarGroupLabel>
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
            <TooltipContent side="right">{t('Theme: {theme}', { theme: t(THEME_META[themeChoice].label) })}</TooltipContent>
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
            aria-label={t('Update download progress')}
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
    const download = onDownloadUpdate;
    const restart = () => void updaterStore.restartApp();
    switch (updater.state) {
      case 'checking':
        return {
          label: `v${version}`, labelClass: 'sidebar-version-label',
          status: t('Checking for updates…'), showProgress: false,
          btnText: t('Checking…'), btnClass: 'sidebar-update-btn', btnDisabled: true, onAction: check,
        };
      case 'available':
        return {
          label: t('Update Available'), labelClass: 'sidebar-version-label highlight-update',
          status: t('v{version} ready to download', { version: updater.version ?? '' }), showProgress: false,
          btnText: t('Review update'), btnClass: 'sidebar-update-btn btn-has-update',
          btnDisabled: false, onAction: download,
        };
      case 'downloading':
        return {
          label: t('Downloading Update'), labelClass: 'sidebar-version-label highlight-update',
          status: t('Downloading {progress}%', { progress: updater.progress }), showProgress: true,
          btnText: t('Downloading {progress}%', { progress: updater.progress }), btnClass: 'sidebar-update-btn',
          btnDisabled: true, onAction: check,
        };
      case 'ready':
        return {
          label: t('Update Ready'), labelClass: 'sidebar-version-label highlight-ready',
          status: t('Restart to apply update'), showProgress: false,
          btnText: t('Restart Fluence'), btnClass: 'sidebar-update-btn btn-ready',
          btnDisabled: false, onAction: restart,
        };
      case 'failed':
        return {
          label: `v${version}`, labelClass: 'sidebar-version-label',
          status: updater.error ? tr(updater.error) : t('Update check failed'), showProgress: false,
          btnText: t('Try Again'), btnClass: 'sidebar-update-btn',
          btnDisabled: false, onAction: check,
        };
      case 'idle':
      default:
        return {
          label: `v${version}`, labelClass: 'sidebar-version-label',
          status: upToDate ? t('✓ Up to date') : null, showProgress: false,
          btnText: t('Check for Updates'), btnClass: 'sidebar-update-btn',
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
        tooltip={t(item.label)}
        data-page={item.page}
        id={`nav-${item.page}`}
        aria-current={active ? 'page' : undefined}
        onClick={() => onNavigate(item.page)}
      >
        <Icon className="nav-icon" aria-hidden="true" />
        <span className="nav-item-label">{t(item.label)}</span>
      </SidebarMenuButton>
    </SidebarMenuItem>
  );
}
