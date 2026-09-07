import { useCallback, useEffect, useState } from 'react';
import { Titlebar } from '@/components/fluence/Titlebar';
import { Sidebar } from '@/components/fluence/Sidebar';
import { AboutPage } from '@/routes/AboutPage';
import { DashboardPage } from '@/routes/DashboardPage';
import { DictionaryPage } from '@/routes/DictionaryPage';
import { GeneralPage } from '@/routes/GeneralPage';
import { HistoryPage } from '@/routes/HistoryPage';
import { ProvidersPage } from '@/routes/ProvidersPage';
import { SnippetsPage } from '@/routes/SnippetsPage';
import { SyncPage } from '@/routes/SyncPage';
import { Toaster } from '@/components/fluence/Toasts';
import { TooltipProvider } from '@/components/ui/tooltip';
import { updaterStore } from '@/ipc/updater';
import { hideMainWindow } from '@/ipc/tauri';
import { isHotkeyRecording } from '@/ipc/general';
import { requestHistorySearchFocus } from '@/ipc/history';

export type Route =
  | 'dashboard'
  | 'history'
  | 'general'
  | 'providers'
  | 'dictionary'
  | 'snippets'
  | 'sync'
  | 'about';

// Same order as vanilla PAGE_ORDER: drives the forward/backward page
// transition direction exactly as before.
const PAGE_ORDER: Route[] = [
  'dashboard',
  'history',
  'general',
  'providers',
  'dictionary',
  'snippets',
  'sync',
  'about',
];

// Main-window shell. Mirrors the vanilla chrome contract:
// custom titlebar, sidebar navigation with View-Transitions page changes,
// focus moved to the new page title for assistive technology.
export function App() {
  const [route, setRoute] = useState<Route>('about');

  // Mirrors vanilla UpdateManager.init() timers (delayed + hourly policy).
  useEffect(() => {
    updaterStore.startBackgroundPolicy();
  }, []);

  // Shell shortcuts from vanilla setupKeyboardShortcuts: Esc hides the
  // window, Ctrl+F/K focuses history search, Ctrl+S quietly flushes the
  // General/Providers pages.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const el = e.target instanceof Element ? e.target : null;
      const tag = el?.tagName;
      const isInput =
        tag === 'INPUT' ||
        tag === 'TEXTAREA' ||
        tag === 'SELECT' ||
        (el instanceof HTMLElement && el.isContentEditable);

      // Escape - close window (a recording hotkey capture consumes Esc
      // first; the shell yields while one is active).
      if (e.key === 'Escape' && !isInput) {
        if (isHotkeyRecording()) return;
        e.preventDefault();
        hideMainWindow().catch(() => undefined);
        return;
      }

      // Ctrl+F / Ctrl+K - focus history search
      if ((e.ctrlKey || e.metaKey) && (e.key === 'f' || e.key === 'k')) {
        e.preventDefault();
        if (route !== 'history') navigateTo('history');
        requestHistorySearchFocus();
        return;
      }

      // Ctrl+S - save current page (quiet flush, no toast — vanilla
      // saveGeneral/saveProviders aliases).
      if ((e.ctrlKey || e.metaKey) && e.key === 's') {
        e.preventDefault();
        if (route === 'general' || route === 'providers') {
          window.dispatchEvent(new CustomEvent('fluence:save-page'));
        }
      }
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [route]);

  const navigateTo = useCallback(
    (page: Route) => {
      if (page === route) return;
      const direction =
        PAGE_ORDER.indexOf(page) > PAGE_ORDER.indexOf(route)
          ? 'forward'
          : 'backward';
      const html = document.documentElement;
      html.classList.add(`nav-${direction}`);
      const apply = () => {
        setRoute(page);
        // Focus lands after paint so screen readers announce the new page.
        requestAnimationFrame(() => {
          document
            .querySelector<HTMLElement>(`#page-${page} .page-title`)
            ?.focus();
        });
      };
      const doc = document as Document & {
        startViewTransition?: (cb: () => void) => { finished: Promise<void> };
      };
      if (doc.startViewTransition) {
        const transition = doc.startViewTransition(apply);
        transition.finished.finally(() => {
          html.classList.remove(`nav-${direction}`);
        });
      } else {
        apply();
        html.classList.remove(`nav-${direction}`);
      }
    },
    [route],
  );

  return (
    <>
      <TooltipProvider>
        <Titlebar />
        <div className="app-shell">
          <Sidebar route={route} onNavigate={navigateTo} />
          <main className="content-area" role="main">
            {route === 'about' ? (
              <AboutPage />
            ) : route === 'dashboard' ? (
              <DashboardPage />
            ) : route === 'sync' ? (
              <SyncPage />
            ) : route === 'dictionary' ? (
              <DictionaryPage />
            ) : route === 'general' ? (
              <GeneralPage />
            ) : route === 'history' ? (
              <HistoryPage />
            ) : route === 'providers' ? (
              <ProvidersPage />
            ) : (
              <SnippetsPage />
            )}
          </main>
        </div>
        <Toaster />
      </TooltipProvider>
    </>
  );
}
