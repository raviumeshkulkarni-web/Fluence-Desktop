import * as React from 'react';
import {
  BookOpen,
  CircleDot,
  Command as CommandIcon,
  History,
  Info,
  LayoutDashboard,
  Minimize2,
  PanelLeftClose,
  PanelLeftOpen,
  RefreshCcw,
  RefreshCw,
  Search,
  Server,
  Settings2,
  Sun,
  Moon,
  X,
  Braces,
} from 'lucide-react';
import {
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandSeparator,
  CommandShortcut,
  highlightMatch,
} from '@/components/ui/command';
import {
  DialogClose,
  DialogFooter,
} from '@/components/ui/dialog';
import { Button } from '@/components/ui/button';
import { hideMainWindow, minimizeMainWindow } from '@/ipc/tauri';
import { updaterStore } from '@/ipc/updater';
import { requestHistorySearchFocus } from '@/ipc/history';
import type { Theme } from '@/lib/theme';
import type { Route } from '@/App';

export interface PaletteAction {
  id: string;
  label: string;
  icon: React.ReactNode;
  shortcut?: string;
  onSelect: () => void;
}

interface CommandPaletteProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  currentRoute: Route;
  onNavigate: (page: Route) => void;
  sidebarCollapsed: boolean;
  onToggleSidebar: () => void;
  theme: Theme;
  onToggleTheme: () => void;
}

const PAGE_META: Record<Route, { label: string; icon: React.ReactNode }> = {
  dashboard: { label: 'Dashboard', icon: <LayoutDashboard className="command-item-icon" /> },
  history: { label: 'History', icon: <History className="command-item-icon" /> },
  general: { label: 'General', icon: <Settings2 className="command-item-icon" /> },
  bubble: { label: 'Floating Bubble', icon: <CircleDot className="command-item-icon" /> },
  providers: { label: 'Providers', icon: <Server className="command-item-icon" /> },
  dictionary: { label: 'Dictionary', icon: <BookOpen className="command-item-icon" /> },
  snippets: { label: 'Snippets', icon: <Braces className="command-item-icon" /> },
  sync: { label: 'Sync', icon: <RefreshCw className="command-item-icon" /> },
  about: { label: 'About', icon: <Info className="command-item-icon" /> },
};

export function CommandPalette({
  open,
  onOpenChange,
  currentRoute,
  onNavigate,
  sidebarCollapsed,
  onToggleSidebar,
  theme,
  onToggleTheme,
}: CommandPaletteProps) {
  const close = () => onOpenChange(false);
  const [search, setSearch] = React.useState('');

  React.useEffect(() => {
    if (!open) setSearch('');
  }, [open]);

  const navigatedRef = React.useRef(false);
  const opener = React.useRef<Element | null>(null);

  React.useEffect(() => {
    if (open) {
      navigatedRef.current = false;
      opener.current = document.activeElement;
    } else if (!navigatedRef.current && opener.current instanceof HTMLElement && opener.current.isConnected) {
      opener.current.focus();
      opener.current = null;
    }
  }, [open]);

  const navigate = (page: Route) => {
    navigatedRef.current = true;
    onNavigate(page);
    close();
  };

  const actions: PaletteAction[] = [
    {
      id: 'refresh-dashboard',
      label: 'Refresh dashboard',
      icon: <RefreshCcw className="command-item-icon" />,
      onSelect: () => {
        window.dispatchEvent(new CustomEvent('fluence:refresh-dashboard'));
        close();
      },
    },
    {
      id: 'toggle-sidebar',
      label: sidebarCollapsed ? 'Expand sidebar' : 'Collapse sidebar',
      icon: sidebarCollapsed
        ? <PanelLeftOpen className="command-item-icon" />
        : <PanelLeftClose className="command-item-icon" />,
      shortcut: 'Ctrl B',
      onSelect: () => {
        onToggleSidebar();
        close();
      },
    },
    {
      id: 'toggle-theme',
      label: theme === 'dark' ? 'Switch to light mode' : 'Switch to dark mode',
      icon: theme === 'dark'
        ? <Sun className="command-item-icon" />
        : <Moon className="command-item-icon" />,
      shortcut: 'Ctrl Shift L',
      onSelect: () => {
        onToggleTheme();
        close();
      },
    },
    {
      id: 'focus-history-search',
      label: 'Focus history search',
      icon: <Search className="command-item-icon" />,
      shortcut: 'Ctrl F',
      onSelect: () => {
        navigate('history');
        window.setTimeout(() => requestHistorySearchFocus(), 50);
      },
    },
    {
      id: 'check-updates',
      label: 'Check for updates',
      icon: <RefreshCw className="command-item-icon" />,
      onSelect: () => {
        void updaterStore.checkForUpdates(true);
        close();
      },
    },
    {
      id: 'minimize',
      label: 'Minimize window',
      icon: <Minimize2 className="command-item-icon" />,
      onSelect: () => {
        void minimizeMainWindow().catch(() => undefined);
        close();
      },
    },
    {
      id: 'hide-window',
      label: 'Hide window',
      icon: <X className="command-item-icon" />,
      shortcut: 'Esc',
      onSelect: () => {
        void hideMainWindow().catch(() => undefined);
        close();
      },
    },
  ];

  const pages = (Object.keys(PAGE_META) as Route[]).map((page) => ({
    id: page,
    label: PAGE_META[page].label,
    icon: PAGE_META[page].icon,
    onSelect: () => navigate(page),
  }));

  return (
    <CommandDialog open={open} onOpenChange={onOpenChange}>
      <CommandInput
        placeholder="Type a command or search…"
        autoFocus
        value={search}
        onValueChange={setSearch}
      />
      <CommandList>
        <CommandEmpty>No matching command found.</CommandEmpty>
        <CommandGroup heading="Actions">
          {actions.map((a) => (
            <CommandItem key={a.id} value={`${a.id} ${a.label}`} onSelect={a.onSelect}>
              {a.icon}
              <span>{highlightMatch(a.label, search)}</span>
              {a.shortcut ? (
                <CommandShortcut>
                  <kbd>{a.shortcut}</kbd>
                </CommandShortcut>
              ) : null}
            </CommandItem>
          ))}
        </CommandGroup>
        <CommandSeparator />
        <CommandGroup heading="Go to">
          {pages.map((p) => (
            <CommandItem
              key={p.id}
              value={`${p.id} ${p.label}`}
              onSelect={p.onSelect}
              data-current={p.id === currentRoute ? 'true' : 'false'}
            >
              {p.icon}
              <span>{highlightMatch(p.label, search)}</span>
              {p.id === currentRoute ? (
                <CommandShortcut>
                  <span className="command-current">Current</span>
                </CommandShortcut>
              ) : null}
            </CommandItem>
          ))}
        </CommandGroup>
      </CommandList>
      <DialogFooter className="command-footer">
        <span className="command-hint">
          <CommandIcon className="command-hint-icon" />
          <kbd>Ctrl K</kbd>
        </span>
        <DialogClose asChild>
          <Button variant="ghost" size="xs">Close</Button>
        </DialogClose>
      </DialogFooter>
    </CommandDialog>
  );
}
