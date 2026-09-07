import * as React from 'react';
import {
  BookOpen,
  Cloud,
  Command,
  History,
  Info,
  LayoutDashboard,
  Minimize2,
  RefreshCcw,
  SearchX,
  Server,
  Settings,
  Sparkles,
  X,
  Zap,
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
} from '@/components/ui/command';
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { Button } from '@/components/ui/button';
import { hideMainWindow, minimizeMainWindow } from '@/ipc/tauri';
import { updaterStore } from '@/ipc/updater';
import { requestHistorySearchFocus } from '@/ipc/history';
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
}

const PAGE_META: Record<Route, { label: string; icon: React.ReactNode }> = {
  dashboard: { label: 'Dashboard', icon: <LayoutDashboard className="command-item-icon" /> },
  history: { label: 'History', icon: <History className="command-item-icon" /> },
  general: { label: 'General', icon: <Settings className="command-item-icon" /> },
  providers: { label: 'Providers', icon: <Server className="command-item-icon" /> },
  dictionary: { label: 'Dictionary', icon: <BookOpen className="command-item-icon" /> },
  snippets: { label: 'Snippets', icon: <Zap className="command-item-icon" /> },
  sync: { label: 'Sync', icon: <Cloud className="command-item-icon" /> },
  about: { label: 'About', icon: <Info className="command-item-icon" /> },
};

export function CommandPalette({
  open,
  onOpenChange,
  currentRoute,
  onNavigate,
}: CommandPaletteProps) {
  const close = () => onOpenChange(false);

  const navigate = (page: Route) => {
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
      id: 'focus-history-search',
      label: 'Focus history search',
      icon: <SearchX className="command-item-icon" />,
      shortcut: 'Ctrl K',
      onSelect: () => {
        navigate('history');
        window.setTimeout(() => requestHistorySearchFocus(), 50);
      },
    },
    {
      id: 'check-updates',
      label: 'Check for updates',
      icon: <Sparkles className="command-item-icon" />,
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
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="command-dialog">
        <DialogHeader className="sr-only">
          <DialogTitle>Command palette</DialogTitle>
          <DialogDescription>
            Search for pages and actions.
          </DialogDescription>
        </DialogHeader>
        <CommandDialog className="command-body">
          <CommandInput placeholder="Type a command or search…" autoFocus />
          <CommandList>
            <CommandEmpty>No matching command found.</CommandEmpty>
            <CommandGroup heading="Actions">
              {actions.map((a) => (
                <CommandItem key={a.id} value={`${a.id} ${a.label}`} onSelect={a.onSelect}>
                  {a.icon}
                  <span>{a.label}</span>
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
                  <span>{p.label}</span>
                  {p.id === currentRoute ? <CommandShortcut>current</CommandShortcut> : null}
                </CommandItem>
              ))}
            </CommandGroup>
          </CommandList>
          <DialogFooter className="command-footer">
            <span className="command-hint">
              <Command className="command-hint-icon" />
              <kbd>Ctrl K</kbd>
            </span>
            <DialogClose asChild>
              <Button variant="ghost" size="xs">Close</Button>
            </DialogClose>
          </DialogFooter>
        </CommandDialog>
      </DialogContent>
    </Dialog>
  );
}
