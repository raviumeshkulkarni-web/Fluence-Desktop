import * as React from 'react';
import { PanelLeft } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Separator } from '@/components/ui/separator';
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from '@/components/ui/tooltip';
import { cn } from '@/lib/cn';

type SidebarState = 'expanded' | 'collapsed';

type SidebarContextValue = {
  state: SidebarState;
  open: boolean;
  setOpen: (open: boolean) => void;
  toggleSidebar: () => void;
};

const SidebarContext = React.createContext<SidebarContextValue | null>(null);

function useSidebar() {
  const context = React.useContext(SidebarContext);
  if (!context) {
    throw new Error('useSidebar must be used within a SidebarProvider');
  }
  return context;
}

type SidebarProviderProps = {
  defaultOpen?: boolean;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  children: React.ReactNode;
};

function SidebarProvider({
  defaultOpen = true,
  open: openProp,
  onOpenChange,
  children,
}: SidebarProviderProps) {
  const [internalOpen, setInternalOpen] = React.useState(defaultOpen);
  const open = openProp ?? internalOpen;
  const setOpen = React.useCallback(
    (nextOpen: boolean) => {
      if (openProp === undefined) {
        setInternalOpen(nextOpen);
      }
      onOpenChange?.(nextOpen);
    },
    [onOpenChange, openProp],
  );
  const toggleSidebar = React.useCallback(() => {
    setOpen(!open);
  }, [open, setOpen]);
  const value = React.useMemo(
    () => ({
      state: open ? ('expanded' as const) : ('collapsed' as const),
      open,
      setOpen,
      toggleSidebar,
    }),
    [open, setOpen, toggleSidebar],
  );

  return <SidebarContext.Provider value={value}>{children}</SidebarContext.Provider>;
}

type SidebarProps = React.ComponentPropsWithoutRef<'div'> & {
  collapsible?: 'icon' | 'none';
};

function Sidebar({ collapsible = 'icon', className, ...props }: SidebarProps) {
  const { state } = useSidebar();
  return (
    <div
      data-slot="sidebar"
      data-state={state}
      data-collapsed={state === 'collapsed' ? 'true' : 'false'}
      data-collapsible={state === 'collapsed' ? collapsible : undefined}
      className={cn('sidebar', state === 'collapsed' && 'collapsed', className)}
      {...props}
    />
  );
}

function SidebarHeader({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      data-slot="sidebar-header"
      data-sidebar="header"
      className={cn('sidebar-header', className)}
      {...props}
    />
  );
}

function SidebarContent({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      data-slot="sidebar-content"
      data-sidebar="content"
      className={cn('sidebar-content', className)}
      {...props}
    />
  );
}

function SidebarGroup({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      data-slot="sidebar-group"
      data-sidebar="group"
      className={cn('sidebar-group', className)}
      {...props}
    />
  );
}

function SidebarGroupLabel({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      data-slot="sidebar-group-label"
      data-sidebar="group-label"
      className={cn('sidebar-group-label', className)}
      {...props}
    />
  );
}

function SidebarMenu({ className, ...props }: React.HTMLAttributes<HTMLUListElement>) {
  return (
    <ul
      data-slot="sidebar-menu"
      data-sidebar="menu"
      className={cn('sidebar-menu', className)}
      {...props}
    />
  );
}

function SidebarMenuItem({ className, ...props }: React.LiHTMLAttributes<HTMLLIElement>) {
  return (
    <li
      data-slot="sidebar-menu-item"
      data-sidebar="menu-item"
      className={cn('sidebar-menu-item', className)}
      {...props}
    />
  );
}

type SidebarMenuButtonProps = React.ButtonHTMLAttributes<HTMLButtonElement> & {
  isActive?: boolean;
  tooltip?: React.ReactNode;
};

function SidebarMenuButton({
  isActive = false,
  tooltip,
  className,
  type = 'button',
  children,
  ...props
}: SidebarMenuButtonProps) {
  const { state } = useSidebar();
  const button = (
    <button
      data-slot="sidebar-menu-button"
      data-sidebar="menu-button"
      data-active={isActive}
      type={type}
      className={cn('nav-item', isActive && 'active', className)}
      {...props}
    >
      {children}
    </button>
  );

  if (tooltip === undefined) {
    return button;
  }

  return (
    <Tooltip>
      <TooltipTrigger asChild>{button}</TooltipTrigger>
      <TooltipContent side="right" hidden={state !== 'collapsed'}>
        {tooltip}
      </TooltipContent>
    </Tooltip>
  );
}

function SidebarFooter({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      data-slot="sidebar-footer"
      data-sidebar="footer"
      className={cn('sidebar-footer', className)}
      {...props}
    />
  );
}

function SidebarSeparator({ className, ...props }: React.ComponentPropsWithoutRef<typeof Separator>) {
  return (
    <Separator
      data-slot="sidebar-separator"
      data-sidebar="separator"
      className={cn('sidebar-separator', className)}
      {...props}
    />
  );
}

type SidebarTriggerProps = React.ComponentPropsWithoutRef<typeof Button> & {
  children?: React.ReactNode;
};

function SidebarTrigger({
  children,
  className,
  onClick,
  type = 'button',
  ...props
}: SidebarTriggerProps) {
  const { toggleSidebar } = useSidebar();
  return (
    <Button
      data-slot="sidebar-trigger"
      data-sidebar="trigger"
      type={type}
      variant="ghost"
      className={cn('sidebar-collapse-toggle', className)}
      onClick={(event) => {
        onClick?.(event);
        if (!event.defaultPrevented) {
          toggleSidebar();
        }
      }}
      {...props}
    >
      {children ?? <PanelLeft data-icon="inline-start" aria-hidden="true" />}
      <span className="sr-only">Toggle Sidebar</span>
    </Button>
  );
}

export {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarProvider,
  SidebarSeparator,
  SidebarTrigger,
  useSidebar,
};
