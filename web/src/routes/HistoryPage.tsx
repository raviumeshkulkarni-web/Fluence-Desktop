import {
  memo,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from 'react';
import { Search, Mic, X, ChevronDown } from 'lucide-react';
import { toast } from '@/components/fluence/Toasts';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { Skeleton } from '@/components/ui/skeleton';
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs';
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuTrigger,
} from '@/components/ui/context-menu';
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
export function countWords(text: string): number {
  return String(text || '')
    .trim()
    .split(/\s+/)
    .filter(Boolean).length;
}

export function historyItemMeta(entry: { text: string; duration_ms: number }): string {
  const parts: string[] = [];
  const words = countWords(entry.text);
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

// Perf: the highlight terms + RegExp depend only on (query, markers),
// which are identical for every row in a render. Building them once per
// render instead of once per row avoids N redundant RegExp compilations
// (50+/page). Output is byte-identical to building per row.
interface HighlightSpec {
  re: RegExp | null;
  terms: Map<string, { id?: string; corrected?: string; isCandidate: boolean }>;
}

function buildHighlightSpec(
  query: string,
  markers: Map<string, PendingMark>,
): HighlightSpec {
  const terms = new Map<string, { id?: string; corrected?: string; isCandidate: boolean }>();
  markers.forEach((info, key) =>
    terms.set(key, { ...info, isCandidate: true }),
  );
  const q = query ? query.trim() : '';
  if (q) terms.set(q.toLowerCase(), { isCandidate: false });

  const pattern = [...terms.keys()]
    .sort((a, b) => b.length - a.length)
    .map(escapeRegExp)
    .join('|');
  if (!pattern) return { re: null, terms };
  return { re: new RegExp(`\\b(${pattern})\\b`, 'gi'), terms };
}

function renderTranscriptTextWithSpec(text: string, spec: HighlightSpec): string {
  const safe = escapeHtml(text);
  if (!spec.re) return safe;
  return safe.replace(spec.re, (match) => {
    const info = spec.terms.get(match.toLowerCase());
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
const DATE_FILTERS = [
  { value: 'all', label: 'All time' },
  { value: 'today', label: 'Today' },
  { value: 'yesterday', label: 'Yesterday' },
] as const;
type DateFilter = (typeof DATE_FILTERS)[number]['value'];

const SORT_OPTIONS = [
  { value: 'newest', label: 'Newest first' },
  { value: 'oldest', label: 'Oldest first' },
  { value: 'longest', label: 'Longest first' },
  { value: 'shortest', label: 'Shortest first' },
  { value: 'words', label: 'Most words' },
  { value: 'fewest', label: 'Fewest words' },
] as const;
type HistorySort = (typeof SORT_OPTIONS)[number]['value'];

// Search text survives route switches (session-only, no storage): remounting
// re-applies it through the normal load path, so box, query, and backend
// filter stay consistent.
let persistedHistorySearch = '';

// Page-0 reload TTL (Dashboard-consistent 30s): focus/event refreshes inside
// this window are skipped — page 0 cannot have meaningfully changed since
// the last successful reset load. Explicit user actions (search, clear,
// delete, accept, clear-all, mount, Load More) bypass the TTL by calling
// load() directly.
const HISTORY_RELOAD_TTL_MS = 30000;

interface HistoryRowProps {
  entry: HistoryEntry;
  expanded: boolean;
  selected: boolean;
  flash: boolean;
  isLast: boolean;
  tabIndex: number;
  highlight: HighlightSpec;
  onToggleExpand: (id: string) => void;
  onToggleSelect: (id: string) => void;
  onSelectRange: (id: string) => void;
  onCopy: (text: string, id: string) => void;
  onDelete: (id: string) => void;
  onFocusRow: (id: string) => void;
  onTextClick: (e: React.MouseEvent) => void;
}

// Memoized row: focus moves, selection, expand, copy-flash, and the
// minute-tick re-render the page — none of them change every row's props,
// so unaffected rows (including their regex highlight + Radix menu) skip
// rendering entirely. Data reloads rebuild entry objects and re-render
// everything, which is correct.
const HistoryRow = memo(function HistoryRow({
  entry,
  expanded,
  selected,
  flash,
  isLast,
  tabIndex,
  highlight,
  onToggleExpand,
  onToggleSelect,
  onSelectRange,
  onCopy,
  onDelete,
  onFocusRow,
  onTextClick,
}: HistoryRowProps) {
  const date = new Date(entry.timestamp);
  return (
    <ContextMenu>
      <ContextMenuTrigger asChild>
        <div
          className={`history-item${flash ? ' copy-flash' : ''}${isLast ? ' is-last' : ''}${expanded ? ' is-expanded' : ''}${selected ? ' is-selected' : ''}`}
          data-history-id={entry.id}
          tabIndex={tabIndex}
          role="button"
          aria-expanded={expanded}
          aria-label={expanded ? 'Collapse transcription' : 'Expand transcription'}
          onFocus={() => onFocusRow(entry.id)}
          onClick={(e) => {
            if (e.ctrlKey || e.metaKey) {
              onToggleSelect(entry.id);
              return;
            }
            if (e.shiftKey) {
              onSelectRange(entry.id);
              return;
            }
            onToggleExpand(entry.id);
          }}
          onKeyDown={(e) => {
            if (e.target !== e.currentTarget) return;
            if ((e.ctrlKey || e.metaKey) && (e.key === ' ' || e.key === 'Spacebar')) {
              e.preventDefault();
              onToggleSelect(entry.id);
              return;
            }
            if (e.key !== 'Enter' && e.key !== ' ') return;
            e.preventDefault();
            onToggleExpand(entry.id);
          }}
        >
          <div className="history-item-header">
            <span className="history-meta-wrap">
              <span className="history-item-time" title={date.toLocaleString()}>
                {formatHistoryTimestamp(entry.timestamp)}
              </span>
              <span className="history-item-meta">{historyItemMeta(entry)}{expanded && entry.provider ? ` · ${entry.provider}` : ''}</span>
            </span>
            <div className="history-actions">
              <span className={`badge badge-${entry.mode === 'agent' ? 'secondary' : 'success'}`}>{entry.mode}</span>
              <Button
                variant="ghost"
                size="xs"
                className="history-copy-btn"
                onClick={(e) => {
                  e.stopPropagation();
                  onCopy(entry.text, entry.id);
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
                  onDelete(entry.id);
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
              __html: renderTranscriptTextWithSpec(entry.text, highlight),
            }}
          />
        </div>
      </ContextMenuTrigger>
      <ContextMenuContent className="history-context-menu">
        <ContextMenuItem
          data-action="copy"
          onClick={() => onCopy(entry.text, entry.id)}
        >
          Copy
        </ContextMenuItem>
        <ContextMenuItem
          data-action="select"
          onClick={() => onToggleSelect(entry.id)}
        >
          {selected ? 'Deselect' : 'Select'}
        </ContextMenuItem>
        <ContextMenuItem
          data-action="delete"
          style={{ color: 'var(--color-error-text)' }}
          onClick={() => onDelete(entry.id)}
        >
          Delete
        </ContextMenuItem>
      </ContextMenuContent>
    </ContextMenu>
  );
});

export function HistoryPage() {
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [markers, setMarkers] = useState<Map<string, PendingMark>>(new Map());
  const [searchInput, setSearchInput] = useState(persistedHistorySearch);
  const [query, setQuery] = useState('');
  const [page, setPage] = useState(0);
  const [lastCount, setLastCount] = useState(0);
  const [loadingMore, setLoadingMore] = useState(false);
  const [flashId, setFlashId] = useState<string | null>(null);
  const [confirmClear, setConfirmClear] = useState(false);
  const [initialLoading, setInitialLoading] = useState(true);
  // P2 filters/sort: all derived locally from loaded rows (category B —
  // no new commands, no schema). Non-default controls trigger a full paging
  // pass with the existing get_history; the paged view returns when cleared.
  const [dateFilter, setDateFilter] = useState<DateFilter>('all');
  const [providerFilter, setProviderFilter] = useState<string>('all');
  const [sort, setSort] = useState<HistorySort>('newest');
  const [collapsedDays, setCollapsedDays] = useState<Set<string>>(new Set());
  const [allEntries, setAllEntries] = useState<HistoryEntry[]>([]);
  const [fullStatus, setFullStatus] = useState<'idle' | 'loading' | 'ready'>('idle');
  const [fullBump, setFullBump] = useState(0);
  const hasActiveFilter = dateFilter !== 'all' || providerFilter !== 'all';
  const fullActive = hasActiveFilter || sort !== 'newest';
  const fullActiveRef = useRef(false);
  fullActiveRef.current = fullActive;
  const queryRef = useRef('');
  const clearFilters = useCallback(() => {
    setDateFilter('all');
    setProviderFilter('all');
  }, []);
  const toggleCollapsedDay = useCallback((dayKey: string) => {
    setCollapsedDays((prev) => {
      const next = new Set(prev);
      if (next.has(dayKey)) next.delete(dayKey);
      else next.add(dayKey);
      return next;
    });
  }, []);
  queryRef.current = query;
  // Expand-in-place (P1): a row click/Enter toggles the full transcript.
  // Copy is explicit (Copy button, context menu, Ctrl+C) so partial text
  // selection works natively instead of firing a whole-row copy.
  const [expandedId, setExpandedId] = useState<string | null>(null);

  const pageRef = useRef(0);
  const searchInputRef = useRef('');
  searchInputRef.current = searchInput;
  const searchTimer = useRef<number | null>(null);
  const searchFieldRef = useRef<HTMLInputElement>(null);
  const flashTimer = useRef<number | null>(null);
  const [, setNowTick] = useState(0);

  // Reload-storm protection: concurrent loads coalesce onto one request,
  // rapid refreshes collapse into a single bounded follow-up, stale
  // responses never overwrite newer results, and page-0 freshness is
  // timestamped for the TTL check in onExternalUpdate.
  const loadSeq = useRef(0);
  const inflightRef = useRef<{
    merged: string;
    reset: boolean;
    promise: Promise<{ marks: Map<string, PendingMark>; list: HistoryEntry[] }>;
  } | null>(null);
  const lastLoadRef = useRef(0);
  const queuedRef = useRef(false);

  const load = useCallback(
    async (reset: boolean, searchArg?: string): Promise<number | null> => {
      const merged = (searchArg ?? '') || searchInputRef.current || '';
      // Same-key coalescing: an identical request is already flying — join
      // it instead of stacking a duplicate IPC pair. (The original applier
      // below writes the shared result; this path only observes its size.)
      const flying = inflightRef.current;
      if (flying && flying.reset === reset && flying.merged === merged) {
        try {
          const r = await flying.promise;
          return r.list.length;
        } catch {
          return null;
        }
      }
      const seq = ++loadSeq.current;
      if (reset) {
        pageRef.current = 0;
        setPage(0);
      }
      setQuery(merged);
      // Suggestions ride alongside history instead of blocking it: both
      // IPCs fly together and their latencies overlap rather than add.
      const work = (async () => {
        // The backend filter must always match the applied query. Non-typing
        // reloads (mount, window focus, history-updated, clear-all) pass no
        // searchArg, so default to the live box text — otherwise the fetch
        // silently goes unfiltered while `query` still shows the old term
        // (stale highlight, broken empty/no-results states).
        const [marks, list] = await Promise.all([
          loadPendingSuggestionMap(),
          getHistory(pageRef.current, merged || null),
        ]);
        return { marks, list };
      })();
      inflightRef.current = { merged, reset, promise: work };
      try {
        const { marks, list } = await work;
        // Stale guard: a newer load started while this one flew — applying
        // these results would overwrite fresher data, so drop them.
        if (seq !== loadSeq.current) return null;
        setMarkers(marks);
        if (reset) {
          setEntries(list);
          lastLoadRef.current = Date.now();
        } else {
          setEntries((prev) => [...prev, ...list]);
        }
        setLastCount(list.length);
        return list.length;
      } catch (err) {
        if (seq !== loadSeq.current) return null;
        toast('Failed to load history: ' + String(err), 'error');
        return null;
      } finally {
        if (inflightRef.current?.promise === work) inflightRef.current = null;
        // One bounded follow-up: refreshes that arrived mid-flight collapse
        // into a single reload instead of stacking. The flag is cleared
        // before running, so this cannot chain.
        if (queuedRef.current) {
          queuedRef.current = false;
          void load(true);
        }
      }
    },
    [],
  );

  // Live-update without yanking the reader (P1): a new dictation while the
  // user sits at the top with no filter simply prepends (scroll preserved);
  // anyone paged, searching, or working a filtered set keeps
  // their position (clearing the filters resyncs via load).
  // Shared core: guards + in-flight queue are identical for both refresh
  // sources; only the TTL differs (see below).
  const refreshFromExternal = useCallback((useTtl: boolean) => {
    if (pageRef.current !== 0 || searchInputRef.current || fullActiveRef.current) return;
    // Storm coalescing: a load is already flying — queue one bounded
    // follow-up (drained in load's finally) instead of stacking another
    // IPC pair per event/focus.
    if (inflightRef.current) {
      queuedRef.current = true;
      return;
    }
    // Short TTL: skip the refresh when page 0 is still fresh. Applies to
    // window-focus returns only — never to mutation events.
    if (useTtl && Date.now() - lastLoadRef.current < HISTORY_RELOAD_TTL_MS) return;
    const top = document.getElementById('history-list')?.scrollTop ?? 0;
    void load(true).then(() => {
      requestAnimationFrame(() => {
        const el = document.getElementById('history-list');
        if (el) el.scrollTop = top;
      });
    });
  }, [load]);

  // Mutation events (save/delete/clear) represent genuine history changes —
  // e.g. a fresh transcription — so they bypass the TTL and always refresh.
  // Rapid bursts stay bounded via the in-flight queue above.
  const onHistoryUpdated = useCallback(() => {
    refreshFromExternal(false);
  }, [refreshFromExternal]);

  // Focus returns carry no mutation signal: refetch only when page 0 is stale.
  const onWindowFocus = useCallback(() => {
    refreshFromExternal(true);
  }, [refreshFromExternal]);

  useEffect(() => {
    void load(true).then(() => setInitialLoading(false));
    let unlisten: (() => void) | undefined;
    void subscribeHistoryUpdated(onHistoryUpdated).then((u) => {
      unlisten = u;
    });
    window.addEventListener('focus', onWindowFocus);
    return () => {
      unlisten?.();
      window.removeEventListener('focus', onWindowFocus);
    };
  }, [load, onHistoryUpdated, onWindowFocus]);

  // Full-set paging for filters/sort (P2): pages 0..n with the applied
  // query through the existing command — always fresh from page 0 so a
  // query change mid-flight can't mix result sets (the effect re-runs and
  // the stale loop abandons itself via the query check).
  useEffect(() => {
    if (!fullActiveRef.current) {
      setFullStatus('idle');
      setAllEntries([]);
      return;
    }
    let cancelled = false;
    setFullStatus('loading');
    setAllEntries([]);
    void (async () => {
      const q = queryRef.current;
      const acc: HistoryEntry[] = [];
      // Date windows are applied server-side (timestamp_ms bounds on the
      // paged reads), so Today/Yesterday never page through the whole table:
      // each fetch is a tight indexed range scan and the loop stops as soon
      // as a short page confirms the window is exhausted. Mirrors the
      // client-side viewEntries filter exactly (t >= t0 for today;
      // t0-day <= t < t0 for yesterday).
      const day = 86400000;
      const start = new Date();
      start.setHours(0, 0, 0, 0);
      const t0 = start.getTime();
      const sinceMs = dateFilter === 'today' ? t0 : dateFilter === 'yesterday' ? t0 - day : undefined;
      const untilMs = dateFilter === 'yesterday' ? t0 : undefined;
      let p = 0;
      for (;;) {
        if (cancelled) return;
        let list: HistoryEntry[];
        try {
          list = await getHistory(p, q || null, sinceMs, untilMs);
        } catch {
          if (!cancelled) setFullStatus('idle');
          return;
        }
        if (cancelled || q !== queryRef.current) return;
        acc.push(...list);
        // Perf: single state commit at the end, not one render + O(n) copy
        // per page. The final list is identical; intermediate commits only
        // caused re-renders of a view still showing its loading state.
        if (list.length < 50) break;
        p += 1;
      }
      if (!cancelled) {
        setAllEntries(acc);
        setFullStatus('ready');
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [fullActive, query, fullBump, dateFilter]);

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

  // Relative timestamps ("5m ago") refresh every minute without a reload.
  useEffect(() => {
    const t = window.setInterval(() => setNowTick((n) => n + 1), 60000);
    return () => window.clearInterval(t);
  }, []);

  // Multi-select (P1): id-based so it survives reloads; anchor enables
  // Shift-click ranges (see selectRange below). Bulk ops reuse the existing
  // single-item commands.
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const selectionAnchor = useRef<string | null>(null);
  const toggleSelect = useCallback((id: string) => {
    selectionAnchor.current = id;
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);
  const clearSelection = useCallback(() => {
    selectionAnchor.current = null;
    setSelectedIds(new Set());
  }, []);
  const toggleExpand = useCallback((id: string) => {
    setExpandedId((prev) => (prev === id ? null : id));
  }, []);
  const focusRow = useCallback((id: string) => {
    setFocusId(id);
  }, []);

  const copyItem = useCallback((text: string, id: string) => {
    // Verbatim vanilla: no rejection handler; failures stay silent.
    copyText(text).then(() => {
      toast('Copied to clipboard', 'success');
      setFlashId(id);
      if (flashTimer.current !== null) window.clearTimeout(flashTimer.current);
      flashTimer.current = window.setTimeout(() => setFlashId(null), 400);
    });
  }, []);

  // Confirm-first delete (cross-platform parity): destructive deletes go
  // through the confirm dialog (Android model) — no grace window, no undo.
  // Rows leave the UI optimistically on confirm; the backend hard-delete
  // fires immediately after. History rows only — the stats ledger is untouched.
  const [confirmDeleteIds, setConfirmDeleteIds] = useState<string[] | null>(null);

  const deleteItems = useCallback((ids: string[]) => {
    if (ids.length === 0) return;
    setConfirmDeleteIds(ids);
  }, []);

  const deleteItem = useCallback((id: string) => deleteItems([id]), [deleteItems]);

  const doConfirmDelete = async () => {
    const ids = confirmDeleteIds;
    setConfirmDeleteIds(null);
    if (!ids || ids.length === 0) return;
    const idSet = new Set(ids);
    // Remove from the working set (filtered full-set when active) and from
    // both lists so every view stays coherent.
    const working = fullActive ? allEntries : entries;
    const doomed = working.filter((e) => idSet.has(e.id));
    if (doomed.length === 0) return;
    setSelectedIds((prev) => {
      const next = new Set(prev);
      doomed.forEach((e) => next.delete(e.id));
      return next;
    });
    setExpandedId((prev) => (prev !== null && idSet.has(prev) ? null : prev));
    setEntries((prev) => prev.filter((e) => !idSet.has(e.id)));
    setAllEntries((prev) => prev.filter((e) => !idSet.has(e.id)));
    if (!fullActive && entries.filter((e) => !idSet.has(e.id)).length === 0 && pageRef.current > 0) {
      // Deleted the last loaded row while paged: restart from page 0 so the
      // mutation-event refresh refills (or truthfully empties) the list
      // instead of leaving it blank.
      pageRef.current = 0;
      setPage(0);
    }
    try {
      for (const e of doomed) await deleteHistoryEntry(e.id);
      toast(
        doomed.length === 1 ? 'Deleted' : `Deleted ${doomed.length} transcriptions`,
        'success',
      );
    } catch (err) {
      toast('Failed to delete: ' + String(err), 'error');
      void load(true);
      // Resync the filtered working set if one is active.
      setFullBump((n) => n + 1);
    }
  };

  const copySelected = useCallback(() => {
    const working = fullActive ? allEntries : entries;
    const sel = working.filter((e) => selectedIds.has(e.id));
    if (sel.length === 0) return;
    // Verbatim single-copy semantics: silent failures stay silent.
    copyText(sel.map((e) => e.text).join('\n\n')).then(() => {
      toast(
        sel.length === 1 ? 'Copied to clipboard' : `Copied ${sel.length} transcriptions`,
        'success',
      );
    });
  }, [fullActive, allEntries, entries, selectedIds]);

  const acceptMark = useCallback(async (id: string | undefined) => {
    if (!id) return;
    // Vanilla resolves the global accept (toast + dictionary refresh live
    // on the Dictionary route) then refreshes markers and this list.
    try {
      await acceptSuggestion(id);
      toast('Added to dictionary', 'success');
    } catch (err) {
      toast('Failed to accept: ' + String(err), 'error');
      return;
    }
    await loadPendingSuggestionMap(true);
    await load(true, searchInputRef.current);
  }, [load]);

  const onSearchInput = (value: string) => {
    setSearchInput(value);
    persistedHistorySearch = value;
    if (searchTimer.current !== null) window.clearTimeout(searchTimer.current);
    searchTimer.current = window.setTimeout(() => void load(true, value), 300);
  };

  const onClearSearch = () => {
    setSearchInput('');
    persistedHistorySearch = '';
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
      persistedHistorySearch = '';
      setSearchInput('');
      setAllEntries([]);
      void load(true);
    } catch (err) {
      toast('Failed to clear history: ' + String(err), 'error');
    }
  };

  // Single shared highlight spec for all rows (see buildHighlightSpec).
  const highlightSpec = useMemo(
    () => buildHighlightSpec(query, markers),
    [query, markers],
  );

  const viewEntries = useMemo(() => {
    const base = fullActive ? allEntries : entries;
    let out = base;
    if (hasActiveFilter) {
      const day = 86400000;
      const start = new Date();
      start.setHours(0, 0, 0, 0);
      const t0 = start.getTime();
      out = out.filter((e) => {
        if (dateFilter !== 'all') {
          const t = +new Date(e.timestamp);
          // DateFilter is only 'today' | 'yesterday' here ('all' is
          // excluded above).
          const ok = dateFilter === 'today' ? t >= t0 : t >= t0 - day && t < t0;
          if (!ok) return false;
        }
        if (providerFilter !== 'all' && e.provider !== providerFilter) return false;
        return true;
      });
    }
    if (sort !== 'newest') {
      // Perf: the comparator runs O(n log n) times and countWords scans the
      // full transcript each call. Cache one count per row — same order,
      // one pass over the texts.
      const wordsCache = new Map<string, number>();
      const wc = (e: HistoryEntry): number => {
        let n = wordsCache.get(e.id);
        if (n === undefined) {
          n = countWords(e.text);
          wordsCache.set(e.id, n);
        }
        return n;
      };
      out = [...out].sort((a, b) =>
        sort === 'oldest'
          ? +new Date(a.timestamp) - +new Date(b.timestamp)
          : sort === 'longest'
            ? b.duration_ms - a.duration_ms
            : sort === 'shortest'
              ? a.duration_ms - b.duration_ms
              : sort === 'words'
                ? wc(b) - wc(a)
                : wc(a) - wc(b),
      );
    }
    return out;
  }, [fullActive, allEntries, entries, hasActiveFilter, dateFilter, providerFilter, sort]);

  // Filter option values come from the working set itself, so a control
  // never offers a value with no rows behind it.
  const providerOptions = useMemo(
    () => [...new Set((fullActive ? allEntries : entries).map((e) => e.provider).filter(Boolean))].sort(),
    [fullActive, allEntries, entries],
  );

  const groups = useMemo(() => {
    const out: { dayKey: string; label: string; items: HistoryEntry[] }[] = [];
    for (const entry of viewEntries) {
      const date = new Date(entry.timestamp);
      const key = dayKeyFor(date);
      const lastGroup = out[out.length - 1];
      if (lastGroup && lastGroup.dayKey === key) lastGroup.items.push(entry);
      else out.push({ dayKey: key, label: historyGroupForDate(date), items: [entry] });
    }
    return out;
  }, [viewEntries]);

  // Rail end-stop (P2): only the last *visible* row, and only when the
  // working set is fully loaded — never while more pages hide behind
  // Load More or a full-set fetch is still running.
  const endReached = fullActive ? fullStatus === 'ready' : lastCount < 50;
  const lastEntryId = (() => {
    if (!endReached) return null;
    for (let gi = groups.length - 1; gi >= 0; gi--) {
      if (collapsedDays.has(groups[gi].dayKey)) continue;
      const items = groups[gi].items;
      if (items.length > 0) return items[items.length - 1].id;
    }
    return null;
  })();

  // Roving tabindex (P1): one Tab stop for the whole list; arrows move
  // within it. flatIndex keeps per-row lookup O(1) as the list grows.
  const [focusId, setFocusId] = useState<string | null>(null);
  const flatIds = useMemo(
    () => groups.flatMap((g) => g.items.map((it) => it.id)),
    [groups],
  );
  const flatIndex = useMemo(() => new Map(flatIds.map((id, i) => [id, i] as const)), [flatIds]);

  const selectRange = useCallback((id: string) => {
    const anchor = selectionAnchor.current ?? id;
    const a = flatIndex.get(anchor);
    const b = flatIndex.get(id);
    if (a === undefined || b === undefined) {
      toggleSelect(id);
      return;
    }
    const [lo, hi] = a < b ? [a, b] : [b, a];
    setSelectedIds((prev) => {
      const next = new Set(prev);
      for (let i = lo; i <= hi; i++) next.add(flatIds[i]);
      return next;
    });
  }, [flatIds, flatIndex, toggleSelect]);

  // Empty states stay truthful under filters: a filter can only narrow what
  // the server returned, so server-empty short-circuits to the base states
  // and the filter-empty state only fires on a non-empty server page.
  const baseEmpty = entries.length === 0 && page === 0;
  const showEmpty = baseEmpty && !query;
  const showNoResults = baseEmpty && !!query;
  const showFilterEmpty =
    !baseEmpty &&
    hasActiveFilter &&
    viewEntries.length === 0 &&
    (!fullActive || fullStatus !== 'loading');
  // The provider control stays mounted so it can animate in/out (CSS
  // max-width transition) instead of popping the row layout on appear.
  const showProviderFilter = providerOptions.length > 1;

  const onTextClick = useCallback((e: React.MouseEvent) => {
    const target = e.target instanceof Element ? e.target : null;
    const mark = target?.closest('.candidate-word');
    if (!mark) return;
    e.stopPropagation();
    void acceptMark(mark.getAttribute('data-suggestion-id') ?? undefined);
  }, [acceptMark]);

  const onListKeyDown = (e: React.KeyboardEvent) => {
    const target = e.target instanceof Element ? e.target : null;

    // Arrow/Home/End navigation across group boundaries (roving tabindex).
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp' || e.key === 'Home' || e.key === 'End') {
      const row = target?.closest?.('.history-item');
      if (!(row instanceof HTMLElement)) return;
      const rows = Array.from(
        document.querySelectorAll<HTMLElement>('#history-list .history-item'),
      );
      const i = rows.indexOf(row);
      if (i < 0) return;
      e.preventDefault();
      let next = i;
      if (e.key === 'ArrowDown') next = Math.min(rows.length - 1, i + 1);
      else if (e.key === 'ArrowUp') next = Math.max(0, i - 1);
      else if (e.key === 'Home') next = 0;
      else next = rows.length - 1;
      rows[next]?.focus();
      return;
    }

    // Ctrl/Cmd+A selects all loaded rows (native select-all wins inside
    // text fields).
    const inField =
      target instanceof HTMLInputElement ||
      target instanceof HTMLTextAreaElement ||
      (target instanceof HTMLElement && target.isContentEditable);
    if ((e.ctrlKey || e.metaKey) && (e.key === 'a' || e.key === 'A') && !inField) {
      e.preventDefault();
      selectionAnchor.current = null;
      setSelectedIds(new Set(flatIds));
      return;
    }

    // Esc layering: collapse expanded row → clear selection → clear search
    // → otherwise let the shell hide the window (existing contract).
    if (e.key === 'Escape') {
      const row = target?.closest?.('.history-item');
      const rowId = row?.getAttribute?.('data-history-id');
      if (rowId && expandedId === rowId) {
        e.preventDefault();
        e.stopPropagation();
        setExpandedId(null);
        return;
      }
      if (selectedIds.size > 0) {
        e.preventDefault();
        e.stopPropagation();
        clearSelection();
        return;
      }
      if (searchInputRef.current) {
        e.preventDefault();
        e.stopPropagation();
        onClearSearch();
        searchFieldRef.current?.focus();
        searchFieldRef.current?.select();
        return;
      }
      return;
    }

    // Ctrl/Cmd+C on a row copies the whole transcript — unless the user
    // selected partial text, in which case native copy wins.
    if ((e.ctrlKey || e.metaKey) && (e.key === 'c' || e.key === 'C')) {
      const sel = window.getSelection();
      if (sel && sel.toString().length > 0) return;
      const row = target?.closest?.('.history-item');
      const rowId = row?.getAttribute?.('data-history-id');
      const found = entries.find((x) => x.id === rowId);
      if (!row || !found) return;
      e.preventDefault();
      e.stopPropagation();
      copyItem(found.text, found.id);
      return;
    }

    // Keyboard context menu: Shift+F10 / Context Menu key opens the row's
    // Radix menu at the row bounds (same focus the mouse right-click uses).
    if (e.key === 'ContextMenu' || (e.shiftKey && e.key === 'F10')) {
      const item = target?.closest?.('.history-item');
      if (!(item instanceof HTMLElement)) return;
      e.preventDefault();
      e.stopPropagation();
      const rect = item.getBoundingClientRect();
      item.dispatchEvent(
        new MouseEvent('contextmenu', {
          bubbles: true,
          cancelable: true,
          view: window,
          clientX: rect.left + Math.min(rect.width, 96),
          clientY: rect.top + Math.min(rect.height, 28),
        })
      );
      return;
    }

    if (e.key !== 'Enter' && e.key !== ' ') return;
    const mark = target?.closest?.('.candidate-word');
    if (!(mark instanceof HTMLElement)) return;
    e.preventDefault();
    e.stopPropagation();
    void acceptMark(mark.dataset.suggestionId);
  };

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
        <div className="history-filter-bar">
          <Tabs value={dateFilter} onValueChange={(v) => setDateFilter(v as DateFilter)}>
            <TabsList aria-label="Date range">
              {DATE_FILTERS.map((f) => (
                <TabsTrigger key={f.value} value={f.value}>
                  {f.label}
                </TabsTrigger>
              ))}
            </TabsList>
          </Tabs>
          <div className="history-filter-controls">
            <div
              className={`history-filter-anim${showProviderFilter ? ' is-visible' : ''}`}
              inert={!showProviderFilter}
            >
              <Select value={providerFilter} onValueChange={setProviderFilter}>
                <SelectTrigger
                  className="select-md"
                  aria-label="Filter by provider"
                >
                  <SelectValue placeholder="Provider" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">All providers</SelectItem>
                  {providerOptions.map((p) => (
                    <SelectItem key={p} value={p}>{p}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <Select value={sort} onValueChange={(v) => setSort(v as HistorySort)}>
              <SelectTrigger
                className="select-md"
                aria-label="Sort transcriptions"
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {SORT_OPTIONS.map((o) => (
                  <SelectItem key={o.value} value={o.value}>{o.label}</SelectItem>
                ))}
              </SelectContent>
            </Select>
            <span className="history-shown-count" aria-live="polite">
              {viewEntries.length === 1
                ? '1 shown'
                : `${viewEntries.length} shown`}
            </span>
          </div>
        </div>
        {selectedIds.size > 0 && (
          <div className="history-selection-bar" role="toolbar" aria-label="Selected transcriptions">
            <span className="history-selection-count">
              {selectedIds.size === 1 ? '1 selected' : `${selectedIds.size} selected`}
            </span>
            <Button variant="ghost" size="xs" onClick={copySelected}>
              Copy{selectedIds.size > 1 ? ` ${selectedIds.size}` : ''}
            </Button>
            <Button
              variant="ghost"
              size="xs"
              style={{ color: 'var(--color-error)' }}
              onClick={() => deleteItems([...selectedIds])}
            >
              Delete{selectedIds.size > 1 ? ` ${selectedIds.size}` : ''}
            </Button>
            <Button variant="ghost" size="xs" onClick={clearSelection}>
              Clear
            </Button>
          </div>
        )}
        <div
          id="history-list"
          className={groups.length <= 1 ? 'single-day' : undefined}
          onKeyDown={onListKeyDown}
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
          {!initialLoading && showFilterEmpty && (
            <div className="empty-state" id="history-no-filter-results">
              <Search className="empty-state-icon" strokeWidth={1.5} aria-hidden="true" />
              <div className="empty-state-title">No matches for these filters</div>
              <div className="empty-state-hint">
                Try widening the date range or clearing the filters.
              </div>
              <Button variant="ghost" size="sm" onClick={clearFilters}>Clear filters</Button>
            </div>
          )}
          {!initialLoading && groups.map((group) => (
            <div key={group.dayKey} role="presentation">
              <button
                type="button"
                className="history-group-header"
                data-day-key={group.dayKey}
                aria-expanded={!collapsedDays.has(group.dayKey)}
                onClick={() => toggleCollapsedDay(group.dayKey)}
              >
                <span className="history-group-label">
                  <ChevronDown
                    size={12}
                    aria-hidden="true"
                    className={collapsedDays.has(group.dayKey) ? 'is-collapsed' : undefined}
                  />
                  {group.label}
                </span>
                <span className="history-group-count">
                  {group.items.length === 1 ? '1 transcription' : `${group.items.length} transcriptions`}
                </span>
              </button>
              {!collapsedDays.has(group.dayKey) && group.items.map((entry) => {
                const rowIndex = flatIndex.get(entry.id) ?? 0;
                const rowTabIndex =
                  focusId != null && flatIndex.has(focusId)
                    ? (entry.id === focusId ? 0 : -1)
                    : (rowIndex === 0 ? 0 : -1);
                return (
                  <HistoryRow
                    key={entry.id}
                    entry={entry}
                    expanded={expandedId === entry.id}
                    selected={selectedIds.has(entry.id)}
                    flash={flashId === entry.id}
                    isLast={lastEntryId === entry.id}
                    tabIndex={rowTabIndex}
                    highlight={highlightSpec}
                    onToggleExpand={toggleExpand}
                    onToggleSelect={toggleSelect}
                    onSelectRange={selectRange}
                    onCopy={copyItem}
                    onDelete={deleteItem}
                    onFocusRow={focusRow}
                    onTextClick={onTextClick}
                  />
                );
              })}
            </div>
          ))}
        </div>
        <div
          style={{ padding: 'var(--spacing-md)', display: 'flex', justifyContent: 'center' }}
          id="history-load-more"
          className={fullActive || lastCount < 50 ? 'hidden' : undefined}
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

      <ConfirmDialog
        open={confirmClear}
        onOpenChange={setConfirmClear}
        title="Clear History"
        body="Clear all transcription history on this device? Statistics are unaffected."
        confirmLabel="Clear All"
        danger
        onConfirm={() => void doClearAll()}
      />
      <ConfirmDialog
        open={confirmDeleteIds !== null}
        onOpenChange={(open) => { if (!open) setConfirmDeleteIds(null); }}
        title={(confirmDeleteIds?.length ?? 0) > 1 ? 'Delete transcriptions' : 'Delete transcription'}
        body={
          (confirmDeleteIds?.length ?? 0) > 1
            ? `This action cannot be undone. Delete ${confirmDeleteIds?.length} transcriptions?`
            : 'This action cannot be undone. Delete this transcription?'
        }
        confirmLabel="Delete"
        danger
        onConfirm={() => void doConfirmDelete()}
      />
    </section>
  );
}
