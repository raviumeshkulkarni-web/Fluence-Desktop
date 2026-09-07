import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from 'react';
import { Search, Mic, X } from 'lucide-react';
import { toast } from '@/components/fluence/Toasts';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Skeleton } from '@/components/ui/skeleton';
import { acceptSuggestion } from '@/ipc/dictionary';
import {
  clearHistory,
  consumeHistorySearchFocus,
  copyText,
  deleteHistoryEntry,
  getHistory,
  loadPendingSuggestionMap,
  subscribeHistoryUpdated,
  type HistoryEntry,
  type PendingMark,
} from '@/ipc/history';

function escapeHtml(str: string): string {
  return String(str)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

function escapeRegExp(str: string): string {
  return str.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

function dayKeyFor(date: Date): string {
  return `${date.getFullYear()}-${date.getMonth()}-${date.getDate()}`;
}

function historyGroupForDate(date: Date): string {
  const now = new Date();
  const todayStart = new Date(
    now.getFullYear(),
    now.getMonth(),
    now.getDate(),
  ).getTime();
  const t = date.getTime();
  if (t >= todayStart) return 'Today';
  if (t >= todayStart - 86400000) return 'Yesterday';
  if (t >= todayStart - 6 * 86400000) {
    return date.toLocaleDateString(undefined, { weekday: 'long' });
  }
  if (date.getFullYear() === now.getFullYear()) {
    return date.toLocaleDateString(undefined, {
      month: 'long',
      day: 'numeric',
    });
  }
  return date.toLocaleDateString(undefined, {
    year: 'numeric',
    month: 'long',
    day: 'numeric',
  });
}

export function formatHistoryTimestamp(ts: string): string {
  const date = new Date(ts);
  const now = new Date();
  const diff = now.getTime() - date.getTime();
  const todayStart = new Date(
    now.getFullYear(),
    now.getMonth(),
    now.getDate(),
  ).getTime();
  const clock = date.toLocaleTimeString(undefined, {
    hour: 'numeric',
    minute: '2-digit',
  });
  if (diff < 60000) return 'Just now';
  if (diff < 3600000) return `${Math.floor(diff / 60000)}m ago`;
  if (date.getTime() >= todayStart) return `${Math.floor(diff / 3600000)}h ago`;
  if (date.getTime() >= todayStart - 86400000) return `Yesterday, ${clock}`;
  if (date.getTime() >= todayStart - 6 * 86400000) {
    return `${date.toLocaleDateString(undefined, { weekday: 'short' })}, ${clock}`;
  }
  return date.toLocaleDateString(undefined, {
    month: 'short',
    day: 'numeric',
    year: date.getFullYear() === now.getFullYear() ? undefined : 'numeric',
  });
}

// Also reused by the Dashboard recent-activity rows (entry shape kept
// structural so both call sites typecheck).
export function historyItemMeta(entry: { text: string; duration_ms: number }): string {
  const parts: string[] = [];
  const words = String(entry.text || '')
    .trim()
    .split(/\s+/)
    .filter(Boolean).length;
  if (words > 0) parts.push(words === 1 ? '1 word' : words + ' words');
  const ms = Number(entry.duration_ms) || 0;
  if (ms > 0) {
    const s = Math.round(ms / 1000);
    parts.push(
      s < 60 ? s + 's' : Math.floor(s / 60) + 'm ' + String(s % 60).padStart(2, '0') + 's',
    );
  }
  // Rendered as React text, not HTML: the parts are counts and fixed words
  // only, so there is nothing to escape — identical output to vanilla's
  // pre-escaped innerHTML.
  return parts.join(' · ');
}

function renderTranscriptText(
  text: string,
  query: string,
  markers: Map<string, PendingMark>,
): string {
  const safe = escapeHtml(text);
  const q = query ? query.trim() : '';
  if (!q && markers.size === 0) return safe;

  const terms = new Map<string, { id?: string; corrected?: string; isCandidate: boolean }>();
  markers.forEach((info, key) =>
    terms.set(key, { ...info, isCandidate: true }),
  );
  if (q) terms.set(q.toLowerCase(), { isCandidate: false });

  const pattern = [...terms.keys()]
    .sort((a, b) => b.length - a.length)
    .map(escapeRegExp)
    .join('|');
  if (!pattern) return safe;

  const re = new RegExp(`\\b(${pattern})\\b`, 'gi');
  return safe.replace(re, (match) => {
    const info = terms.get(match.toLowerCase());
    if (info?.isCandidate) {
      const title = `Suggestion: replace with '${info.corrected}' (click to accept)`;
      return `<mark class="candidate-word" data-suggestion-id="${info.id}" role="button" tabindex="0" title="${escapeHtml(title)}" aria-label="${escapeHtml(title)}">${match}</mark>`;
    }
    return `<mark>${match}</mark>`;
  });
}

// Faithful port of the vanilla History surface (#page-history +
// setupHistory/loadHistory/renderHistoryItem, candidate-word markers,
// context menu, search + load-more + clear-all). Same DOM ids/classes,
// same copy, same toasts, same 50-item paging. Day-group headers, counts,
// the is-last rail stop, and the single-day class are derived during
// render instead of via post-render DOM passes — identical output while
// items exist.
export function HistoryPage() {
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [markers, setMarkers] = useState<Map<string, PendingMark>>(new Map());
  const [searchInput, setSearchInput] = useState('');
  const [query, setQuery] = useState('');
  const [page, setPage] = useState(0);
  const [lastCount, setLastCount] = useState(0);
  const [loadingMore, setLoadingMore] = useState(false);
  const [flashId, setFlashId] = useState<string | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; rowId: string } | null>(null);
  const [menuPos, setMenuPos] = useState<{ left: number; top: number } | null>(null);
  const [confirmClear, setConfirmClear] = useState(false);
  const [initialLoading, setInitialLoading] = useState(true);

  const pageRef = useRef(0);
  const searchInputRef = useRef('');
  searchInputRef.current = searchInput;
  const entriesRef = useRef<HistoryEntry[]>([]);
  entriesRef.current = entries;
  const searchTimer = useRef<number | null>(null);
  const searchFieldRef = useRef<HTMLInputElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const flashTimer = useRef<number | null>(null);

  const load = useCallback(
    async (reset: boolean, searchArg?: string): Promise<number | null> => {
      const merged = (searchArg ?? '') || searchInputRef.current || '';
      if (reset) {
        pageRef.current = 0;
        setPage(0);
      }
      setQuery(merged);
      try {
        const marks = await loadPendingSuggestionMap();
        const list = await getHistory(pageRef.current, searchArg || null);
        setMarkers(marks);
        if (reset) setEntries(list);
        else setEntries((prev) => [...prev, ...list]);
        setLastCount(list.length);
        return list.length;
      } catch (err) {
        toast('Failed to load history: ' + String(err), 'error');
        return null;
      }
    },
    [],
  );

  useEffect(() => {
    void load(true).then(() => setInitialLoading(false));
    let unlisten: (() => void) | undefined;
    void subscribeHistoryUpdated(() => void load(true)).then((u) => {
      unlisten = u;
    });
    const onFocus = () => void load(true);
    window.addEventListener('focus', onFocus);
    return () => {
      unlisten?.();
      window.removeEventListener('focus', onFocus);
    };
  }, [load]);

  // Ctrl+F/K focus request from the shell (vanilla focuses + selects).
  useEffect(() => {
    const focusSearch = () => {
      searchFieldRef.current?.focus();
      searchFieldRef.current?.select();
    };
    if (consumeHistorySearchFocus()) focusSearch();
    const onFocusSearch = () => {
      consumeHistorySearchFocus();
      focusSearch();
    };
    window.addEventListener('fluence:focus-history-search', onFocusSearch);
    return () =>
      window.removeEventListener('fluence:focus-history-search', onFocusSearch);
  }, []);

  useEffect(
    () => () => {
      if (searchTimer.current !== null) window.clearTimeout(searchTimer.current);
      if (flashTimer.current !== null) window.clearTimeout(flashTimer.current);
    },
    [],
  );

  const hideMenu = useCallback(() => setMenu(null), []);

  const hideMenuAndRefocus = useCallback(() => {
    setMenu((current) => {
      if (current) {
        const row = document.querySelector(
          `[data-history-id="${current.rowId}"]`,
        );
        if (row instanceof HTMLElement) row.focus();
      }
      return null;
    });
  }, []);

  // While the context menu is open, any click/scroll dismisses it and Esc
  // closes it with focus back on the row. The keydown listener uses the
  // capture phase so it preempts the shell Esc handler exactly like
  // vanilla's stopImmediatePropagation ordering.
  useEffect(() => {
    if (!menu) return;
    const onClick = () => hideMenu();
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation();
        hideMenuAndRefocus();
      }
    };
    const onScroll = () => hideMenu();
    document.addEventListener('click', onClick);
    document.addEventListener('keydown', onKeyDown, true);
    window.addEventListener('scroll', onScroll, true);
    return () => {
      document.removeEventListener('click', onClick);
      document.removeEventListener('keydown', onKeyDown, true);
      window.removeEventListener('scroll', onScroll, true);
    };
  }, [menu, hideMenu, hideMenuAndRefocus]);

  // Clamp the menu into the viewport after mount, before paint.
  useLayoutEffect(() => {
    if (!menu) {
      setMenuPos(null);
      return;
    }
    const rect = menuRef.current?.getBoundingClientRect();
    if (!rect) {
      setMenuPos({ left: menu.x, top: menu.y });
      return;
    }
    setMenuPos({
      left: Math.min(menu.x, window.innerWidth - rect.width - 8),
      top: Math.min(menu.y, window.innerHeight - rect.height - 8),
    });
  }, [menu]);

  const copyItem = (text: string, id: string) => {
    // Verbatim vanilla: no rejection handler; failures stay silent.
    copyText(text).then(() => {
      toast('Copied to clipboard', 'success');
      setFlashId(id);
      if (flashTimer.current !== null) window.clearTimeout(flashTimer.current);
      flashTimer.current = window.setTimeout(() => setFlashId(null), 400);
    });
  };

  const deleteItem = async (id: string) => {
    try {
      await deleteHistoryEntry(id);
      setEntries((prev) => prev.filter((e) => e.id !== id));
      toast('Deleted', 'success');
    } catch (err) {
      toast('Failed to delete: ' + String(err), 'error');
    }
  };

  const acceptMark = async (id: string | undefined) => {
    if (!id) return;
    // Vanilla resolves the global accept (toast + dictionary refresh live
    // on the Dictionary route) then refreshes markers and this list.
    try {
      await acceptSuggestion(id);
      toast('Added to dictionary ✓', 'success');
    } catch (err) {
      toast('Failed to accept: ' + String(err), 'error');
      return;
    }
    await loadPendingSuggestionMap(true);
    await load(true, searchInputRef.current);
  };

  const openMenu = (rowId: string | undefined, x: number, y: number) => {
    if (!rowId) return;
    setMenu({ x, y, rowId });
  };

  const onSearchInput = (value: string) => {
    setSearchInput(value);
    if (searchTimer.current !== null) window.clearTimeout(searchTimer.current);
    searchTimer.current = window.setTimeout(() => void load(true, value), 300);
  };

  const onClearSearch = () => {
    setSearchInput('');
    void load(true, '');
  };

  const onLoadMore = async () => {
    if (loadingMore) return;
    setLoadingMore(true);
    pageRef.current += 1;
    setPage(pageRef.current);
    try {
      const n = await load(false, searchInputRef.current);
      // A failed fetch must not consume the page, or the next retry
      // would silently skip a page of results.
      if (n === null) {
        pageRef.current = Math.max(0, pageRef.current - 1);
        setPage(pageRef.current);
      }
    } finally {
      setLoadingMore(false);
    }
  };

  const doClearAll = async () => {
    try {
      await clearHistory();
      toast('History cleared', 'success');
      void load(true);
    } catch (err) {
      toast('Failed to clear history: ' + String(err), 'error');
    }
  };

  const groups = useMemo(() => {
    const out: { dayKey: string; label: string; items: HistoryEntry[] }[] = [];
    for (const entry of entries) {
      const date = new Date(entry.timestamp);
      const key = dayKeyFor(date);
      const lastGroup = out[out.length - 1];
      if (lastGroup && lastGroup.dayKey === key) lastGroup.items.push(entry);
      else out.push({ dayKey: key, label: historyGroupForDate(date), items: [entry] });
    }
    return out;
  }, [entries]);

  const lastEntryId =
    groups.length > 0
      ? groups[groups.length - 1].items[groups[groups.length - 1].items.length - 1]?.id
      : null;

  const showEmpty = entries.length === 0 && page === 0 && !query;
  const showNoResults = entries.length === 0 && page === 0 && !!query;

  const onTextClick = (e: React.MouseEvent) => {
    const target = e.target instanceof Element ? e.target : null;
    const mark = target?.closest('.candidate-word');
    if (!mark) return;
    e.stopPropagation();
    void acceptMark(mark.getAttribute('data-suggestion-id') ?? undefined);
  };

  const onListKeyDown = (e: React.KeyboardEvent) => {
    const target = e.target instanceof Element ? e.target : null;
    if (e.key === 'F10' && e.shiftKey) {
      const row =
        target?.closest?.('.history-item') ??
        document.activeElement?.closest?.('.history-item');
      if (!(row instanceof HTMLElement)) return;
      e.preventDefault();
      const rect = row.getBoundingClientRect();
      openMenu(row.dataset.historyId, rect.left + 8, rect.top);
      return;
    }
    if (e.key !== 'Enter' && e.key !== ' ') return;
    const mark = target?.closest?.('.candidate-word');
    if (!(mark instanceof HTMLElement)) return;
    e.preventDefault();
    e.stopPropagation();
    void acceptMark(mark.dataset.suggestionId);
  };

  const onListContextMenu = (e: React.MouseEvent) => {
    const target = e.target instanceof Element ? e.target : null;
    const row = target?.closest('.history-item');
    if (!(row instanceof HTMLElement)) return;
    e.preventDefault();
    openMenu(row.dataset.historyId, e.clientX, e.clientY);
  };

  const menuEntry = menu
    ? entriesRef.current.find((e) => e.id === menu.rowId)
    : undefined;

  return (
    <section className="page active" id="page-history">
      <div className="page-header">
        <h1 className="page-title" tabIndex={-1}>History</h1>
        <p className="page-subtitle">Browse and search every transcription on this device</p>
      </div>

      <div className="settings-section" style={{ overflow: 'hidden' }}>
        <div className="settings-section-header" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
          <h2>Recent Transcriptions</h2>
          <Button variant="danger" size="xs" id="clear-history-btn" onClick={() => setConfirmClear(true)}>Clear All</Button>
        </div>
        <div className="search-wrapper">
          <Search size={15} strokeWidth={2} aria-hidden="true" />
          <Input
            ref={searchFieldRef}
            type="search"
            id="history-search"
            placeholder="Search transcriptions…"
            aria-label="Search transcriptions"
            value={searchInput}
            onChange={(e) => onSearchInput(e.target.value)}
          />
        </div>
        <div
          id="history-list"
          style={{ maxHeight: 400, overflowY: 'auto' }}
          className={groups.length <= 1 ? 'single-day' : undefined}
          onKeyDown={onListKeyDown}
          onContextMenu={onListContextMenu}
        >
          {initialLoading && (
            <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--spacing-sm)', padding: 'var(--spacing-md)' }}>
              <Skeleton style={{ height: 64 }} />
              <Skeleton style={{ height: 64 }} />
              <Skeleton style={{ height: 64 }} />
            </div>
          )}
          {!initialLoading && showEmpty && (
            <div className="empty-state" id="history-empty">
              <Mic className="empty-state-icon" strokeWidth={1.5} aria-hidden="true" />
              <div className="empty-state-title">No transcriptions yet</div>
              <div className="empty-state-hint">Press <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Space</kbd> to start your first transcription</div>
            </div>
          )}
          {!initialLoading && showNoResults && (
            <div className="empty-state" id="history-no-results">
              <Search className="empty-state-icon" strokeWidth={1.5} aria-hidden="true" />
              <div className="empty-state-title">No matches</div>
              <div className="empty-state-hint" id="history-no-results-hint">
                {query ? `Nothing matches "${query}" on this device.` : 'Nothing matches your search.'}
              </div>
              <Button variant="ghost" size="sm" id="history-clear-search-btn" onClick={onClearSearch}>Clear search</Button>
            </div>
          )}
          {!initialLoading && groups.map((group) => (
            <div key={group.dayKey}>
              <div className="history-group-header" data-day-key={group.dayKey}>
                <span>{group.label}</span>
                <span className="history-group-count">
                  {group.items.length === 1 ? '1 transcription' : `${group.items.length} transcriptions`}
                </span>
              </div>
              {group.items.map((entry) => {
                const date = new Date(entry.timestamp);
                return (
                  <div
                    key={entry.id}
                    className={`history-item${flashId === entry.id ? ' copy-flash' : ''}${lastEntryId === entry.id ? ' is-last' : ''}`}
                    data-history-id={entry.id}
                    tabIndex={0}
                    role="button"
                    aria-label="Copy transcription to clipboard"
                    onClick={() => copyItem(entry.text, entry.id)}
                    onKeyDown={(e) => {
                      if (e.target !== e.currentTarget) return;
                      if (e.key !== 'Enter' && e.key !== ' ') return;
                      e.preventDefault();
                      copyItem(entry.text, entry.id);
                    }}
                  >
                    <div className="history-item-header">
                      <span className="history-meta-wrap">
                        <span className="history-item-time" title={date.toLocaleString()}>
                          {formatHistoryTimestamp(entry.timestamp)}
                        </span>
                        <span className="history-item-meta">{historyItemMeta(entry)}</span>
                      </span>
                      <div className="history-actions">
                        <span className={`badge badge-${entry.mode === 'agent' ? 'primary' : 'success'}`}>{entry.mode}</span>
                        <Button
                          variant="ghost"
                          size="xs"
                          className="history-copy-btn"
                          onClick={(e) => {
                            e.stopPropagation();
                            copyItem(entry.text, entry.id);
                          }}
                        >
                          Copy
                        </Button>
                        <Button
                          variant="ghost"
                          size="xs"
                          className="history-delete-btn"
                          data-history-id={entry.id}
                          aria-label="Delete transcription"
                          style={{ color: 'var(--color-error)' }}
                          onClick={(e) => {
                            e.stopPropagation();
                            void deleteItem(entry.id);
                          }}
                        >
                          <X size={12} aria-hidden="true" />
                        </Button>
                      </div>
                    </div>
                    <div
                      className="history-item-text"
                      onClick={onTextClick}
                      dangerouslySetInnerHTML={{
                        __html: renderTranscriptText(entry.text, query, markers),
                      }}
                    />
                  </div>
                );
              })}
            </div>
          ))}
        </div>
        <div
          style={{ padding: 'var(--spacing-md)', display: 'flex', justifyContent: 'center' }}
          id="history-load-more"
          className={lastCount < 50 ? 'hidden' : undefined}
        >
          <Button
            variant="ghost"
            id="load-more-btn"
            disabled={loadingMore}
            onClick={() => void onLoadMore()}
          >
            {loadingMore ? 'Loading…' : 'Load More'}
          </Button>
        </div>
      </div>

      {menu && (
        <div
          ref={menuRef}
          className="history-context-menu"
          role="menu"
          style={
            menuPos
              ? { position: 'fixed', left: menuPos.left, top: menuPos.top }
              : { position: 'fixed', left: menu.x, top: menu.y }
          }
          onClick={(e) => e.stopPropagation()}
          onContextMenu={(e) => e.preventDefault()}
          onKeyDown={(e) => {
            if (e.key === 'Escape') {
              e.stopPropagation();
              hideMenuAndRefocus();
            }
          }}
        >
          <button
            type="button"
            role="menuitem"
            data-action="copy"
            autoFocus
            onClick={() => {
              if (menuEntry) copyItem(menuEntry.text, menuEntry.id);
              hideMenu();
            }}
          >
            Copy
          </button>
          <button
            type="button"
            role="menuitem"
            data-action="delete"
            onClick={() => {
              void deleteItem(menu.rowId);
              hideMenu();
            }}
          >
            Delete
          </button>
        </div>
      )}

      <ConfirmDialog
        open={confirmClear}
        onOpenChange={setConfirmClear}
        title="Clear History"
        body="Clear all transcription history?"
        confirmLabel="Clear All"
        danger
        onConfirm={() => void doClearAll()}
      />
    </section>
  );
}
