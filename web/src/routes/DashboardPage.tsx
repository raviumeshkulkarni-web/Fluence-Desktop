import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import type { MouseEvent as ReactMouseEvent } from 'react';
import { Area, AreaChart, Bar, BarChart, CartesianGrid, Cell, Pie, PieChart, Sector, XAxis, YAxis } from 'recharts';
import {
  Copy,
  Minus,
  MoreHorizontal,
  RefreshCw,
  TrendingDown,
  TrendingUp,
} from 'lucide-react';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import { Button } from '@/components/ui/button';
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card';
import { Badge } from '@/components/ui/badge';
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { Skeleton } from '@/components/ui/skeleton';
import { ChartContainer } from '@/components/ui/chart';
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table';
import { toast } from '@/components/fluence/Toasts';
import {
  getAccountActivity,
  type DailyBucket,
} from '@/ipc/dashboard';
import { copyText, subscribeHistoryUpdated } from '@/ipc/history';
import { subscribeSyncStatus } from '@/ipc/sync';
import type { Theme } from '@/lib/theme';

const DAY_MS = 86400000;

// THE Kotlin parity contract: UTC-midnight day starts; weeks aggregate
// Monday-to-Sunday by UTC Monday; sums compose across days/weeks/months
// identically from the same bucket rows. No local-timezone math anywhere.
function utcDayStart(ms: number): number {
  return Math.floor(ms / DAY_MS) * DAY_MS;
}

function formatDurationMs(ms: number): string {
  if (!(ms > 0)) {
    return '0m';
  }
  const totalMinutes = Math.round(ms / 60000);
  const hours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  if (hours > 0) {
    return minutes > 0 ? `${hours}h ${minutes}m` : `${hours}h`;
  }
  return `${minutes}m`;
}

function formatTotalWords(totalWords: number): string {
  if (totalWords >= 1000000) {
    return (totalWords / 1000000).toFixed(1) + 'M';
  } else if (totalWords >= 1000) {
    return (totalWords / 1000).toFixed(1) + 'K';
  }
  return totalWords.toLocaleString();
}

// Vanilla animateStatValue: 700ms cubic ease-out count-up preserving
// decimals/suffix/thousands separators. Instant when reduced-motion is
// set, the text is non-numeric, the target is 0, or already showing.
function AnimatedStat({ id, value }: { id: string; value: string }) {
  const [display, setDisplay] = useState(value);
  const displayRef = useRef(display);
  displayRef.current = display;
  const rafRef = useRef<number | null>(null);

  useEffect(() => {
    if (displayRef.current === value) return;
    const reduced =
      typeof window.matchMedia === 'function' &&
      window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    const match = String(value).match(/^([\d.,]+)\s*(.*)$/);
    if (reduced || !match) {
      setDisplay(value);
      return;
    }
    const target = parseFloat(match[1].replace(/,/g, ''));
    const suffix = match[2];
    const decimals = match[1].includes('.') ? match[1].split('.')[1].length : 0;
    if (target === 0) {
      setDisplay(value);
      return;
    }
    const duration = 700;
    const start = performance.now();
    const step = (now: number) => {
      const t = Math.min(1, (now - start) / duration);
      const eased = 1 - Math.pow(1 - t, 3);
      const v = target * eased;
      setDisplay(
        (decimals > 0 ? v.toFixed(decimals) : Math.round(v).toLocaleString()) +
          suffix,
      );
      if (t < 1) {
        rafRef.current = requestAnimationFrame(step);
      } else {
        setDisplay(value);
        rafRef.current = null;
      }
    };
    rafRef.current = requestAnimationFrame(step);
    return () => {
      if (rafRef.current !== null) cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    };
  }, [value]);

  return (
    <div className="kpi-value" id={id}>
      {display}
    </div>
  );
}

interface DashboardKpi {
  id: string;
  title: string;
  value: string;
  foot: string;
}

type Range = '7d' | '30d' | '90d' | 'all';

// Chart metric: area reads the sessions trend, bars read discrete words per
// bucket. One toggle switches both type and metric together (never a 2x2
// matrix). Both series derive from the same synced ledger buckets; words use
// the same whitespace-delimited counting on both platforms.
type Metric = 'sessions' | 'words';

const METRIC_KEY = 'fluence_chart_metric';

function readMetric(): Metric {
  try {
    return window.localStorage.getItem(METRIC_KEY) === 'words' ? 'words' : 'sessions';
  } catch {
    return 'sessions';
  }
}

interface RangePoint {
  label: string;
  count: number;
  words: number;
}

interface WindowTotals {
  sessions: number;
  words: number;
  durationMs: number;
}

// Sum buckets with day_start_ms in [startMs, endMs). Pure UTC range math.
function sumWindow(
  buckets: DailyBucket[],
  startMs: number,
  endMs: number,
): WindowTotals {
  let sessions = 0;
  let words = 0;
  let durationMs = 0;
  for (const b of buckets) {
    if (b.day_start_ms >= startMs && b.day_start_ms < endMs) {
      sessions += b.sessions;
      words += b.words;
      durationMs += b.duration_ms;
    }
  }
  return { sessions, words, durationMs };
}

function spokenLabel(ms: number): string {
  const hours = ms / 3600000;
  return hours >= 1
    ? hours.toFixed(1) + 'h spoken'
    : Math.round(hours * 60) + 'm spoken';
}

// Re-derive one range view from cached buckets (no IPC): daily points for
// 7/30/90 days, adaptive daily/weekly/monthly for All-time (capped so
// recharts stays flat at any history length).
function viewData(buckets: DailyBucket[], range: Range): RangePoint[] {
  const today = utcDayStart(Date.now());
  if (range === 'all') {
    const first = buckets.length > 0 ? buckets[0].day_start_ms : today;
    const spanDays = Math.max(1, Math.round((today - first) / DAY_MS) + 1);
    if (spanDays <= 92) {
      return dailyPoints(buckets, spanDays, (d) =>
        d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' }),
      );
    }
    if (spanDays <= 730) {
      const weeks = new Map<number, { sessions: number; words: number }>();
      for (const b of buckets) {
        const monday = b.day_start_ms - ((new Date(b.day_start_ms).getUTCDay() + 6) % 7) * DAY_MS;
        const prev = weeks.get(monday) ?? { sessions: 0, words: 0 };
        weeks.set(monday, { sessions: prev.sessions + b.sessions, words: prev.words + b.words });
      }
      return [...weeks.entries()]
        .sort((a, b) => a[0] - b[0])
        .map(([monday, totals]) => ({
          label: new Date(monday).toLocaleDateString(undefined, {
            month: 'short',
            day: 'numeric',
          }),
          count: totals.sessions,
          words: totals.words,
        }));
    }
    const months = new Map<number, { sessions: number; words: number }>();
    for (const b of buckets) {
      const d = new Date(b.day_start_ms);
      const key = d.getUTCFullYear() * 12 + d.getUTCMonth();
      const prev = months.get(key) ?? { sessions: 0, words: 0 };
      months.set(key, { sessions: prev.sessions + b.sessions, words: prev.words + b.words });
    }
    return [...months.entries()]
      .sort((a, b) => a[0] - b[0])
      .map(([key, totals]) => ({
        label: new Date(Date.UTC(Math.floor(key / 12), key % 12, 1)).toLocaleDateString(
          undefined,
          { month: 'short', year: 'numeric' },
        ),
        count: totals.sessions,
        words: totals.words,
      }));
  }
  const n = range === '7d' ? 7 : range === '30d' ? 30 : 90;
  return dailyPoints(buckets, n, (d) =>
    range === '7d'
      ? d.toLocaleDateString(undefined, { weekday: 'short' })
      : d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' }),
  );
}

function dailyPoints(
  buckets: DailyBucket[],
  n: number,
  label: (d: Date) => string,
): RangePoint[] {
  const byDay = new Map(buckets.map((b) => [b.day_start_ms, b]));
  const today = utcDayStart(Date.now());
  return Array.from({ length: n }, (_, k) => {
    const dayMs = today - (n - 1 - k) * DAY_MS;
    const d = new Date(dayMs);
    const bucket = byDay.get(dayMs);
    return {
      label: label(d),
      count: bucket?.sessions ?? 0,
      words: bucket?.words ?? 0,
    };
  });
}

// Rounds up to the next 1/1.5/2/2.5/3/4/5/6/8/10 x 10^k step, so the shared
// ceiling is always a clean number, always leaves headroom above the peak (a
// peak stroke at the exact domain max would be clipped by the plot edge), and
// yields 4-6 gridlines instead of the float ceilings a raw x1.05 produces.
function niceCeiling(v: number): number {
  if (!(v > 0)) return 1;
  const mag = Math.pow(10, Math.floor(Math.log10(v)));
  for (const step of [1, 1.5, 2, 2.5, 3, 4, 5, 6, 8, 10]) {
    const ceiling = step * mag;
    if (ceiling > v) return ceiling;
  }
  return 10 * mag;
}

interface WeekdayPoint {
  weekday: string;
  full: string;
  sessions: number;
  words: number;
}

// Fixed Monday-first weekday labels derived from a known Monday
// (2024-01-01) so order is always Mon -> Sun and names stay locale-aware.
// Module scope: the values are constant for the session.
const WEEKDAY_LABELS: { short: string[]; full: string[] } = (() => {
  const short: string[] = [];
  const full: string[] = [];
  for (let i = 0; i < 7; i += 1) {
    const d = new Date(Date.UTC(2024, 0, 1 + i));
    short.push(d.toLocaleDateString(undefined, { weekday: 'short', timeZone: 'UTC' }));
    full.push(d.toLocaleDateString(undefined, { weekday: 'long', timeZone: 'UTC' }));
  }
  return { short, full };
})();

// Additive weekday distribution: folds the same synced DailyBucket[] rows
// into exactly seven Monday-first buckets (never sorted by value).
// O(days), never O(events); no new IPC, no history reads.
function weekdayData(buckets: DailyBucket[], range: Range): WeekdayPoint[] {
  const { short, full } = WEEKDAY_LABELS;
  const points: WeekdayPoint[] = short.map((weekday, i) => ({
    weekday,
    full: full[i],
    sessions: 0,
    words: 0,
  }));
  const today = utcDayStart(Date.now());
  const n = range === '7d' ? 7 : range === '30d' ? 30 : range === '90d' ? 90 : 0;
  const startMs = n > 0 ? today - (n - 1) * DAY_MS : 0;
  const endMs = today + DAY_MS;
  for (const b of buckets) {
    if (b.day_start_ms < startMs || b.day_start_ms >= endMs) continue;
    // getUTCDay: 0 = Sunday. Shift so Monday = 0 .. Sunday = 6.
    const idx = (new Date(b.day_start_ms).getUTCDay() + 6) % 7;
    points[idx].sessions += b.sessions;
    points[idx].words += b.words;
  }
  return points;
}

// Interpolates the chart duo (a -> b -> c) into n slice fills so the donut
// stays in the existing chart language without introducing new tokens.
function duoRamp(duo: { a: string; b: string; c: string }, n: number): string[] {
  const toRgb = (hex: string): [number, number, number] => {
    const v = parseInt(hex.slice(1), 16);
    return [(v >> 16) & 255, (v >> 8) & 255, v & 255];
  };
  const mix = (from: string, to: string, t: number): string => {
    const f = toRgb(from);
    const e = toRgb(to);
    const c = f.map((x, i) => Math.round(x + (e[i] - x) * t));
    return `#${c.map((x) => x.toString(16).padStart(2, '0')).join('')}`;
  };
  return Array.from({ length: n }, (_, i) => {
    const t = n < 2 ? 0 : i / (n - 1);
    return t < 0.5 ? mix(duo.a, duo.b, t * 2) : mix(duo.b, duo.c, (t - 0.5) * 2);
  });
}

// Donut sector shape (recharts v3 `shape` API). One STABLE component for the
// lifetime of the page: the active index is read from a ref rather than closed
// over, so hovering never changes the component identity and the 7 sectors are
// re-rendered instead of unmounted/remounted. Expansion is instant (no
// transition), so reduced-motion needs no special-casing.
function makeWeekdaySectorShape(activeIndexRef: { current: number | null }) {
  return function WeekdaySector(props: any) {
    const active = props.index === activeIndexRef.current;
    return (
      <Sector
        {...props}
        outerRadius={props.outerRadius + (active ? 6 : 0)}
        cornerRadius={active ? 4 : 0}
      />
    );
  };
}

// Session-count momentum badge for the active range. All-time and
// uncovered prior windows show nothing rather than a fabricated delta.
function TrendBadge({
  buckets,
  range,
}: {
  buckets: DailyBucket[];
  range: Range;
}) {
  const span = range === '7d' ? 7 : range === '30d' ? 30 : range === '90d' ? 90 : 0;
  if (buckets.length === 0 || span === 0) return null;
  const today = utcDayStart(Date.now());
  const firstDay = buckets[0].day_start_ms;
  // The prior window must be fully covered, or the delta would mislead.
  if (firstDay > today - (2 * span - 1) * DAY_MS) return null;
  const current = sumWindow(buckets, today - (span - 1) * DAY_MS, today + DAY_MS);
  const prior = sumWindow(
    buckets,
    today - (2 * span - 1) * DAY_MS,
    today - (span - 1) * DAY_MS,
  );
  const delta = current.sessions - prior.sessions;
  const variant = delta > 0 ? 'success' : delta < 0 ? 'destructive' : 'secondary';
  const Icon = delta > 0 ? TrendingUp : delta < 0 ? TrendingDown : Minus;
  const text = `${delta > 0 ? '+' : delta < 0 ? '-' : '±'}${delta !== 0 ? Math.abs(delta) : '0'} vs prior ${span}d`;
  return (
    <Badge variant={variant}>
      <Icon size={12} aria-hidden="true" />
      {text}
    </Badge>
  );
}

// Module cache (stale-then-reload across remounts, no re-skeleton).
let cachedBuckets: DailyBucket[] | null = null;
let dashboardLoaded = false;
let skeletonCleared = false;

export function DashboardPage({ theme = 'dark' }: { theme?: Theme }) {
  const [buckets, setBuckets] = useState<DailyBucket[]>(cachedBuckets ?? []);
  const [loaded, setLoaded] = useState(dashboardLoaded);
  const [skeleton, setSkeleton] = useState(!skeletonCleared);
  const [range, setRange] = useState<Range>('7d');
  const [metric, setMetric] = useState<Metric>(readMetric);

  useEffect(() => {
    try {
      window.localStorage.setItem(METRIC_KEY, metric);
    } catch {
      // Persistence is an enhancement; the chart still works without it.
    }
  }, [metric]);
  const bucketsRef = useRef(buckets);
  bucketsRef.current = buckets;
  const lastFetchRef = useRef(0);
  const inflightRef = useRef<Promise<void> | null>(null);
  const accountRef = useRef<string | null | undefined>(undefined);
  const reduced = useMemo(
    () =>
      typeof window.matchMedia === 'function' &&
      window.matchMedia('(prefers-reduced-motion: reduce)').matches,
    [],
  );

  // Chart duo follows the settings-shell theme (owner direction: muted
  // amethyst-to-cyan, desaturated, no glow). Light stops deepen for white.
  const duo =
    theme === 'dark'
      ? { a: '#8E7CC3', b: '#7498C6', c: '#5FB4C2' }
      : { a: '#6E5AA8', b: '#4E7FA8', c: '#2E8B99' };
  const themedConfig = useMemo(
    () => ({
      sessions: {
        label: 'Sessions',
        color: theme === 'dark' ? '#0BD6E3' : '#0E7490',
      },
    }),
    [theme],
  );

  // Full authoritative snapshot (mount, account switch): replaces the
  // cache. Refresh paths use topUp below and merge by day instead.
  const fullLoad = useCallback(async () => {
    const list = await getAccountActivity(undefined);
    cachedBuckets = [...list].sort((a, b) => a.day_start_ms - b.day_start_ms);
    setBuckets(cachedBuckets);
    lastFetchRef.current = Date.now();
    dashboardLoaded = true;
    setLoaded(true);
  }, []);

  const topUp = useCallback(async () => {
    const have = bucketsRef.current;
    const since = have.length > 0 ? have[0].day_start_ms : undefined;
    const list = await getAccountActivity(since);
    const merged = new Map(have.map((b) => [b.day_start_ms, b]));
    list.forEach((b) => merged.set(b.day_start_ms, b));
    cachedBuckets = [...merged.values()].sort((a, b) => a.day_start_ms - b.day_start_ms);
    setBuckets(cachedBuckets);
    lastFetchRef.current = Date.now();
    dashboardLoaded = true;
    setLoaded(true);
  }, []);

  // 30s freshness TTL + in-flight coalescing: focus/event/refresh storms
  // collapse into at most one request per window.
  const refresh = useCallback(async () => {
    if (inflightRef.current) {
      await inflightRef.current.catch(() => undefined);
      return;
    }
    if (Date.now() - lastFetchRef.current < 30_000 && bucketsRef.current.length > 0) {
      return;
    }
    const p = topUp().catch((err) => {
      console.error('Failed to load dashboard stats:', err);
    });
    inflightRef.current = p;
    try {
      await p;
    } finally {
      if (inflightRef.current === p) inflightRef.current = null;
    }
  }, [topUp]);

  useEffect(() => {
    let cancelled = false;
    fullLoad()
      .catch((err) => console.error('Failed to load dashboard stats:', err))
      .finally(() => {
        skeletonCleared = true;
        if (!cancelled) setSkeleton(false);
      });
    const onRefresh = () => void refresh();
    window.addEventListener('fluence:refresh-dashboard', onRefresh);
    let unlistenHistory: (() => void) | undefined;
    void subscribeHistoryUpdated(() => void refresh()).then((u) => {
      unlistenHistory = u;
    });
    let unlistenSync: (() => void) | undefined;
    // Account switch: drop the other account's buckets, full reload.
    void subscribeSyncStatus((s) => {
      const key = s?.account_key ?? null;
      if (accountRef.current === undefined) {
        accountRef.current = key;
        return;
      }
      if (key !== accountRef.current) {
        accountRef.current = key;
        cachedBuckets = null;
        setBuckets([]);
        lastFetchRef.current = 0;
        void fullLoad().catch((err) =>
          console.error('Failed to load dashboard stats:', err),
        );
      } else {
        void refresh();
      }
    }).then((u) => {
      unlistenSync = u;
    });
    const onFocus = () => void refresh();
    window.addEventListener('focus', onFocus);
    return () => {
      cancelled = true;
      unlistenHistory?.();
      unlistenSync?.();
      window.removeEventListener('focus', onFocus);
      window.removeEventListener('fluence:refresh-dashboard', onRefresh);
    };
  }, [fullLoad, refresh]);

  const summary = useMemo(() => {
    const today = utcDayStart(Date.now());
    const lifetime = sumWindow(buckets, 0, today + DAY_MS);
    const last7 = sumWindow(buckets, today - 6 * DAY_MS, today + DAY_MS);
    const last30 = sumWindow(buckets, today - 29 * DAY_MS, today + DAY_MS);
    const last90 = sumWindow(buckets, today - 89 * DAY_MS, today + DAY_MS);
    return { lifetime, last7, last30, last90 };
  }, [buckets]);

  const kpis = useMemo((): DashboardKpi[] => {
    const totals =
      range === '7d'
        ? { ...summary.last7, scope: 'in last 7 days' }
        : range === '30d'
          ? { ...summary.last30, scope: 'in last 30 days' }
          : range === '90d'
            ? { ...summary.last90, scope: 'in last 90 days' }
            : { ...summary.lifetime, scope: 'all time' };
    return [
      {
        id: 'stat-total-words',
        title: 'Words Transcribed',
        value: formatTotalWords(totals.words),
        foot: `${totals.sessions.toLocaleString()} sessions, ${totals.scope}`,
      },
      {
        id: 'stat-time-saved',
        title: 'Typing Time Saved',
        value: formatDurationMs((totals.words / 40) * 60000),
        foot: `at 40 WPM, ${totals.scope}`,
      },
      {
        id: 'stat-dictation-time',
        title: 'Dictation Time',
        value: formatDurationMs(totals.durationMs),
        foot: `${spokenLabel(totals.durationMs)}, ${totals.scope}`,
      },
      {
        id: 'stat-sessions',
        title: 'Sessions',
        value: totals.sessions.toLocaleString(),
        foot: totals.scope === 'all time' ? 'all time' : `in last ${range === '7d' ? 7 : range === '30d' ? 30 : 90} days`,
      },
    ];
  }, [summary, range]);

  const points = useMemo(() => viewData(buckets, range), [buckets, range]);
  const activeValue = (p: RangePoint) => (metric === 'sessions' ? p.count : p.words);
  const rangeTotal = points.reduce((a, p) => a + activeValue(p), 0);
  // Additive weekday card: same buckets, same range, same metric toggle.
  // Independent of the hero series (never reads points, which lose weekday
  // detail under all-time weekly/monthly aggregation).
  const weekdayPoints = useMemo(() => weekdayData(buckets, range), [buckets, range]);
  const weekdayValue = (p: WeekdayPoint) => (metric === 'sessions' ? p.sessions : p.words);
  const weekdayTotal = weekdayPoints.reduce((a, p) => a + weekdayValue(p), 0);
  const weekdayPeak = weekdayPoints.reduce((best, p) =>
    weekdayValue(p) > weekdayValue(best) ? p : best,
  );
  const rangeLabel =
    range === '7d' ? 'Last 7 days' : range === '30d' ? 'Last 30 days' : range === '90d' ? 'Last 90 days' : 'All time';
  const weekdayUnit = (v: number) =>
    metric === 'sessions' ? `session${v === 1 ? '' : 's'}` : `word${v === 1 ? '' : 's'}`;
  const weekdayPeakShare =
    weekdayTotal > 0 ? Math.round((weekdayValue(weekdayPeak) / weekdayTotal) * 100) : 0;
  const weekdaySummary =
    weekdayTotal === 0
      ? `No activity in this range yet`
      : `Peak ${weekdayPeak.full} ${weekdayPeakShare}% · ${weekdayValue(weekdayPeak).toLocaleString()} ${weekdayUnit(weekdayValue(weekdayPeak))} · ${rangeLabel.toLowerCase()}`;
  // Slice fills interpolated from the existing chart duo (no new tokens).
  // Depend on `theme`, not the `duo` object literal, which is rebuilt every
  // render and would defeat the memo.
  const weekdayFills = useMemo(
    () => duoRamp(duo, weekdayPoints.length),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [theme, weekdayPoints.length],
  );

  // Additive momentum ring: today vs trailing daily average from the same
  // synced buckets. The average covers up to 30 prior calendar days (fewer
  // for new accounts, labelled honestly); a null ratio means no baseline yet
  // rather than a fabricated comparison.
  const todayStart = utcDayStart(Date.now());
  const todayBucket = buckets.find((b) => b.day_start_ms === todayStart);
  const todayValue =
    metric === 'sessions' ? (todayBucket?.sessions ?? 0) : (todayBucket?.words ?? 0);
  const trailStart = todayStart - 30 * DAY_MS;
  const coverageStart =
    buckets.length > 0 ? Math.max(trailStart, buckets[0].day_start_ms) : todayStart;
  let trailSum = 0;
  for (const b of buckets) {
    if (b.day_start_ms >= coverageStart && b.day_start_ms < todayStart) {
      trailSum += metric === 'sessions' ? b.sessions : b.words;
    }
  }
  const trailDays = Math.max(1, Math.round((todayStart - coverageStart) / DAY_MS));
  const trailAvg = trailSum / trailDays;
  const todayRatio = trailAvg > 0 ? todayValue / trailAvg : null;
  const ringSweep = todayRatio == null ? 0 : Math.min(1, todayRatio);
  const todaySummary =
    todayRatio == null
      ? `No baseline yet — dictate to build your average`
      : `${todayRatio.toFixed(1)}× your daily average`;
  // Recharts does not auto-size the axis: 4-digit counts overflow a 32px
  // gutter and the SVG viewport clips their leading digit.
  const yWidth = points.some((p) => activeValue(p) >= 1000) ? 44 : 32;

  // Android-parity hover chip: anchored to the active data point, clamped
  // horizontally into the plot, flipping below the point when there is no
  // room above. Driven by native mouse position (no recharts event API),
  // so the geometry is explicit: left gutter = yWidth, right/top margins
  // mirror the AreaChart margin prop, X labels occupy ~30px at the bottom.
  const hostRef = useRef<HTMLDivElement>(null);
  const chipRef = useRef<HTMLDivElement>(null);
  const [hoverIdx, setHoverIdx] = useState<number | null>(null);
  // Donut/table link: hovering a slice highlights its table row and vice
  // versa. Indexes the fixed Mon -> Sun slots, so it stays meaningful across
  // metric/range changes (no reset needed). The ref mirrors the state so the
  // stable sector shape can read it without changing component identity.
  const [activeWeekday, setActiveWeekdayState] = useState<number | null>(null);
  const activeWeekdayRef = useRef<number | null>(null);
  const weekdaySectorShape = useMemo(
    () => makeWeekdaySectorShape(activeWeekdayRef),
    [],
  );
  const setActiveWeekday = useCallback((index: number | null) => {
    activeWeekdayRef.current = index;
    setActiveWeekdayState(index);
  }, []);
  const [chipBox, setChipBox] = useState({ w: 0, h: 0 });
  useLayoutEffect(() => {
    const el = chipRef.current;
    if (el == null) return;
    const w = el.offsetWidth;
    const h = el.offsetHeight;
    setChipBox((prev) => (prev.w === w && prev.h === h ? prev : { w, h }));
  });
  const onPlotMove = (e: ReactMouseEvent) => {
    const el = hostRef.current;
    if (el == null || points.length === 0) {
      setHoverIdx(null);
      return;
    }
    const rect = el.getBoundingClientRect();
    const plotL = yWidth;
    const plotR = rect.width - 8;
    const span = Math.max(1, plotR - plotL);
    // Area points sit on the plot edges; bars sit at band centers. The
    // hover mapping must use the same layout or the dot drifts off the bar.
    const frac =
      metric === 'sessions'
        ? (e.clientX - rect.left - plotL) / span
        : (e.clientX - rect.left - plotL) / span - 0.5 / points.length;
    const denom = metric === 'sessions' ? points.length - 1 : points.length;
    const idx = Math.round(frac * denom);
    setHoverIdx(Math.min(points.length - 1, Math.max(0, idx)));
  };
  const hovered = hoverIdx != null && points[hoverIdx] != null ? points[hoverIdx] : null;
  const hostW = hostRef.current?.offsetWidth ?? 0;
  const hostH = hostRef.current?.offsetHeight ?? 0;
  // ONE y scale shared by the axis and the hover math. Recharts would
  // otherwise round the domain up to a "nice" maximum of its own choosing
  // (220 for a 200 peak), so the hand-computed dot Y used a different scale
  // than the rendered line and drifted off it — sometimes touching, sometimes
  // floating clear of the stroke. Both sides read this one value.
  const yScaleMax = useMemo(
    () => niceCeiling(Math.max(0, ...points.map((p) => activeValue(p)))),
    [points, metric],
  );
  const hoverGeom = (() => {
    if (hovered == null || hoverIdx == null || hostW <= 0 || hostH <= 0) return null;
    const plotL = yWidth;
    const plotR = hostW - 8;
    const plotT = 8;
    const plotB = hostH - 30;
    const frac =
      points.length < 2
        ? 0.5
        : metric === 'sessions'
          ? hoverIdx / (points.length - 1)
          : (hoverIdx + 0.5) / points.length;
    const dotX = plotL + frac * Math.max(0, plotR - plotL);
    const dotY = plotT + (1 - activeValue(hovered) / yScaleMax) * Math.max(0, plotB - plotT);
    const left =
      chipBox.w >= plotR - plotL
        ? plotL
        : Math.min(Math.max(dotX - chipBox.w / 2, plotL), plotR - chipBox.w);
    const above = dotY - 15 - chipBox.h;
    return { dotX, dotY, plotT, plotB, left, top: above < 0 ? dotY + 15 : above };
  })();

  const copyValue = (value: string) => {
    copyText(value).then(() => toast('Copied to clipboard', 'success'));
  };

  return (
    <section className="page active" id="page-dashboard">
      <div className="page-header" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'flex-start', gap: 'var(--spacing-md)' }}>
        <div>
          <h1 className="page-title" tabIndex={-1}>Dashboard</h1>
          <p className="page-subtitle">Your transcription activity at a glance</p>
        </div>
        <Tooltip>
          <TooltipTrigger asChild>
            <Button
              variant="ghost"
              size="sm"
              id="dashboard-refresh-btn"
              aria-label="Refresh dashboard"
              onClick={() => window.dispatchEvent(new CustomEvent('fluence:refresh-dashboard'))}
            >
              <RefreshCw size={14} aria-hidden="true" />
              Refresh
            </Button>
          </TooltipTrigger>
          <TooltipContent>Refresh dashboard</TooltipContent>
        </Tooltip>
      </div>

      <div className="kpi-grid">
        {skeleton && buckets.length === 0
          ? ['stat-total-words', 'stat-time-saved', 'stat-dictation-time', 'stat-sessions'].map(
              (id) => (
                <Card key={id}>
                  <CardHeader>
                    <Skeleton style={{ height: 14, width: '55%' }} />
                  </CardHeader>
                  <CardContent>
                    <Skeleton className="kpi-skel" />
                    <Skeleton style={{ height: 12, width: '70%' }} />
                  </CardContent>
                </Card>
              ),
            )
          : kpis.map((kpi) => (
              <Card key={kpi.id}>
                <CardHeader>
                  <CardTitle>{kpi.title}</CardTitle>
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild>
                      <Button
                        variant="ghost"
                        size="xs"
                        aria-label={`${kpi.title} options`}
                      >
                        <MoreHorizontal size={14} aria-hidden="true" />
                      </Button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      <DropdownMenuItem onSelect={() => copyValue(kpi.value)}>
                        <Copy size={14} aria-hidden="true" />
                        Copy value
                      </DropdownMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>
                </CardHeader>
                <CardContent>
                  <AnimatedStat id={kpi.id} value={kpi.value} />
                  <div className="kpi-trendrow">
                    {/* Reserved badge slot: the pill (or its absence) must never
                        change card height. The slot always occupies the first
                        line at badge height (12px icon + 8px padding + 2px
                        border), the foot always the second, so badge/no-badge
                        states have identical geometry and the page can't jump. */}
                    <span
                      style={{
                        display: 'flex',
                        alignItems: 'center',
                        flexBasis: '100%',
                        minHeight: 22,
                      }}
                    >
                      <TrendBadge buckets={buckets} range={range} />
                    </span>
                    <div className="kpi-foot">{kpi.foot}</div>
                  </div>
                </CardContent>
              </Card>
            ))}
      </div>

      <div style={{ marginTop: 'var(--spacing-md)', display: 'flex', flexDirection: 'column', flex: '1 0 auto' }}>
        <Card className="chart-fill">
          <CardHeader>
            <div>
              <CardTitle>Activity</CardTitle>
              <CardDescription>
                {metric === 'sessions' ? 'Transcription sessions' : 'Words transcribed'}
              </CardDescription>
            </div>
            <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--spacing-sm)' }}>
            <ToggleGroup
              type="single"
              className="tabs-list"
              value={metric}
              onValueChange={(v) => {
                if (v) setMetric(v as Metric);
              }}
              aria-label="Chart metric"
            >
              <ToggleGroupItem value="sessions" className="tabs-trigger">Sessions</ToggleGroupItem>
              <ToggleGroupItem value="words" className="tabs-trigger">Words</ToggleGroupItem>
            </ToggleGroup>
            <ToggleGroup
              type="single"
              className="tabs-list"
              value={range}
              onValueChange={(v) => {
                if (v) setRange(v as Range);
              }}
              aria-label="Activity range"
            >
              <ToggleGroupItem value="7d" className="tabs-trigger">Last 7 days</ToggleGroupItem>
              <ToggleGroupItem value="30d" className="tabs-trigger">Last 30 days</ToggleGroupItem>
              <ToggleGroupItem value="90d" className="tabs-trigger">Last 90 days</ToggleGroupItem>
              <ToggleGroupItem value="all" className="tabs-trigger">All time</ToggleGroupItem>
            </ToggleGroup>
            </div>
          </CardHeader>
          <CardContent>
            {loaded && rangeTotal === 0 ? (
              <div className="chart-empty" id="chart-empty">
                No activity in this range yet. Press your hotkey to dictate.
              </div>
            ) : (
              <div
                ref={hostRef}
                className="chart-hover-host"
                onMouseMove={onPlotMove}
                onMouseLeave={() => setHoverIdx(null)}
              >
              <ChartContainer config={themedConfig} height="100%">
                {metric === 'sessions' ? (
                <AreaChart data={points} margin={{ top: 8, right: 8, bottom: 0, left: 0 }}>
                  <defs>
                    <linearGradient id="dashAreaGrad" x1="0" y1="0" x2="0" y2="1">
                      <stop offset="0%" stopColor={duo.a} stopOpacity={0.22} />
                      <stop offset="50%" stopColor={duo.b} stopOpacity={0.1} />
                      <stop offset="100%" stopColor={duo.c} stopOpacity={0.03} />
                    </linearGradient>
                    <linearGradient id="dashStrokeGrad" x1="0" y1="0" x2="1" y2="0">
                      <stop offset="0%" stopColor={duo.a} />
                      <stop offset="55%" stopColor={duo.b} />
                      <stop offset="100%" stopColor={duo.c} />
                    </linearGradient>
                  </defs>
                  <CartesianGrid vertical={false} stroke="rgba(255,255,255,0.06)" />
                  <XAxis
                    dataKey="label"
                    tickLine={false}
                    axisLine={false}
                    minTickGap={24}
                    tick={{ fill: '#A0A0A0', fontSize: 12 }}
                  />
                  <YAxis
                    allowDecimals={false}
                    width={yWidth}
                    domain={[0, yScaleMax]}
                    tickLine={false}
                    axisLine={false}
                    tick={{ fill: '#A0A0A0', fontSize: 12 }}
                    tickFormatter={(v: number) => v.toLocaleString()}
                  />
                  <Area
                    type="monotone"
                    dataKey="count"
                    name="sessions"
                    stroke="url(#dashStrokeGrad)"
                    strokeWidth={2}
                    fill="url(#dashAreaGrad)"
                    dot={false}
                    activeDot={false}
                    isAnimationActive={!reduced}
                    animationDuration={225}
                  />
                </AreaChart>
                ) : (
                <BarChart data={points} margin={{ top: 8, right: 8, bottom: 0, left: 0 }}>
                  <defs>
                    <linearGradient id="dashBarGrad" x1="0" y1="0" x2="0" y2="1">
                      <stop offset="0%" stopColor={duo.a} stopOpacity={0.9} />
                      <stop offset="100%" stopColor={duo.c} stopOpacity={0.9} />
                    </linearGradient>
                  </defs>
                  <CartesianGrid vertical={false} stroke="rgba(255,255,255,0.06)" />
                  <XAxis
                    dataKey="label"
                    tickLine={false}
                    axisLine={false}
                    minTickGap={24}
                    tick={{ fill: '#A0A0A0', fontSize: 12 }}
                  />
                  <YAxis
                    allowDecimals={false}
                    width={yWidth}
                    domain={[0, yScaleMax]}
                    tickLine={false}
                    axisLine={false}
                    tick={{ fill: '#A0A0A0', fontSize: 12 }}
                    tickFormatter={(v: number) => v.toLocaleString()}
                  />
                  <Bar
                    dataKey="words"
                    name="words"
                    fill="url(#dashBarGrad)"
                    radius={[3, 3, 0, 0]}
                    maxBarSize={26}
                    activeBar={false}
                    isAnimationActive={!reduced}
                    animationDuration={225}
                  />
                </BarChart>
                )}
              </ChartContainer>
              {hovered != null && hoverGeom != null && (
                <div className="chart-hover-layer" aria-hidden="true">
                  <div
                    className="chart-crosshair"
                    style={{
                      left: hoverGeom.dotX,
                      top: hoverGeom.plotT,
                      height: Math.max(0, hoverGeom.plotB - hoverGeom.plotT),
                    }}
                  />
                  <div
                    className="chart-hover-dot"
                    style={{ left: hoverGeom.dotX - 4, top: hoverGeom.dotY - 4 }}
                  />
                  <div
                    ref={chipRef}
                    className="chart-tooltip chart-hover-chip"
                    style={{ left: hoverGeom.left, top: hoverGeom.top }}
                  >
                    <span className="chart-tooltip-value">
                      {activeValue(hovered).toLocaleString()}{' '}
                      {metric === 'sessions'
                        ? `session${activeValue(hovered) === 1 ? '' : 's'}`
                        : `word${activeValue(hovered) === 1 ? '' : 's'}`}{', '}
                    </span>
                    <span className="chart-tooltip-label">{hovered.label}</span>
                  </div>
                </div>
              )}
              </div>
            )}
          </CardContent>
        </Card>
        <div
          style={{
            marginTop: 'var(--spacing-md)',
            display: 'flex',
            gap: 'var(--spacing-md)',
            flexWrap: 'wrap',
          }}
        >
        <Card style={{ flex: '2 1 340px', minWidth: 0 }}>
          <CardHeader>
            <div>
              <CardTitle>By weekday</CardTitle>
              <CardDescription>
                {metric === 'sessions' ? 'Sessions share by weekday' : 'Words share by weekday'} · {weekdaySummary}
              </CardDescription>
            </div>
          </CardHeader>
          <CardContent>
            {weekdayTotal === 0 ? (
              <div className="chart-empty">
                No activity in this range yet. Press your hotkey to dictate.
              </div>
            ) : (
              <figure style={{ margin: 0 }}>
                <div
                  style={{
                    display: 'flex',
                    flexWrap: 'wrap',
                    alignItems: 'center',
                    gap: 'var(--spacing-md)',
                  }}
                >
                  <div style={{ flex: '0 1 240px', minWidth: 200, position: 'relative' }}>
                  <ChartContainer config={themedConfig} height={200}>
                    <PieChart onMouseLeave={() => setActiveWeekday(null)}>
                      <Pie
                        data={weekdayPoints}
                        dataKey={metric === 'sessions' ? 'sessions' : 'words'}
                        nameKey="full"
                        innerRadius="58%"
                        outerRadius="88%"
                        paddingAngle={2}
                        strokeWidth={0}
                        isAnimationActive={!reduced}
                        animationDuration={225}
                        shape={weekdaySectorShape}
                        onMouseEnter={(_, i) => setActiveWeekday(i)}
                        onMouseLeave={() => setActiveWeekday(null)}
                      >
                        {weekdayPoints.map((p, i) => (
                          <Cell key={p.full} fill={weekdayFills[i]} />
                        ))}
                      </Pie>
                    </PieChart>
                  </ChartContainer>
                  {/* Center readout: total by default, hovered slice detail on
                      hover. Pointer-transparent so slice hover keeps working;
                      the Table below carries the same data for SR/keyboard. */}
                  <div
                    aria-hidden="true"
                    style={{
                      position: 'absolute',
                      inset: 0,
                      display: 'flex',
                      flexDirection: 'column',
                      alignItems: 'center',
                      justifyContent: 'center',
                      textAlign: 'center',
                      pointerEvents: 'none',
                    }}
                  >
                    {activeWeekday != null && weekdayPoints[activeWeekday] != null ? (
                      <>
                        <span style={{ fontSize: 12, color: 'var(--color-on-surface-variant)' }}>
                          {weekdayPoints[activeWeekday].full}
                        </span>
                        <span
                          style={{
                            fontFamily: 'var(--font-display)',
                            fontSize: 17,
                            fontWeight: 600,
                            color: 'var(--color-on-surface)',
                            fontVariantNumeric: 'tabular-nums',
                          }}
                        >
                          {weekdayValue(weekdayPoints[activeWeekday]).toLocaleString()}
                        </span>
                        <span style={{ fontSize: 11, color: 'var(--color-on-surface-variant)' }}>
                          {weekdayTotal > 0
                            ? Math.round(
                                (weekdayValue(weekdayPoints[activeWeekday]) / weekdayTotal) * 100,
                              )
                            : 0}
                          %
                        </span>
                      </>
                    ) : (
                      <>
                        <span
                          style={{
                            fontFamily: 'var(--font-display)',
                            fontSize: 17,
                            fontWeight: 600,
                            color: 'var(--color-on-surface)',
                            fontVariantNumeric: 'tabular-nums',
                          }}
                        >
                          {weekdayTotal.toLocaleString()}
                        </span>
                        <span style={{ fontSize: 11, color: 'var(--color-on-surface-variant)' }}>
                          {metric === 'sessions' ? 'sessions' : 'words'} total
                        </span>
                      </>
                    )}
                  </div>
                  </div>
                  <div className="table-card" style={{ flex: '1 1 220px', minWidth: 0, alignSelf: 'center' }}>
                  <Table>
                    {/* Caller-owned column widths (the component fixes layout
                        only): 3-char day names need little room, right-aligned
                        numbers need the remainder split so the Day|Value and
                        Value|Share gutters measure equal. */}
                    <colgroup>
                      <col style={{ width: '22%' }} />
                      <col style={{ width: '38%' }} />
                      <col style={{ width: '40%' }} />
                    </colgroup>
                    <TableHeader>
                      <TableRow>
                        <TableHead style={{ padding: '7px 10px' }}>Day</TableHead>
                        <TableHead style={{ padding: '7px 10px', textAlign: 'right' }}>
                          {metric === 'sessions' ? 'Sessions' : 'Words'}
                        </TableHead>
                        <TableHead style={{ padding: '7px 10px', textAlign: 'right' }}>Share</TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {weekdayPoints.map((p, i) => {
                        const share =
                          weekdayTotal > 0 ? Math.round((weekdayValue(p) / weekdayTotal) * 100) : 0;
                        return (
                          <TableRow
                            key={p.full}
                            data-state={activeWeekday === i ? 'selected' : undefined}
                            onMouseEnter={() => setActiveWeekday(i)}
                            onMouseLeave={() => setActiveWeekday(null)}
                          >
                            <TableCell style={{ padding: '7px 10px', whiteSpace: 'nowrap' }}>
                              {p.weekday}
                            </TableCell>
                            <TableCell
                              style={{
                                padding: '7px 10px',
                                textAlign: 'right',
                                fontVariantNumeric: 'tabular-nums',
                              }}
                            >
                              {weekdayValue(p).toLocaleString()}
                            </TableCell>
                            <TableCell
                              style={{
                                padding: '7px 10px',
                                textAlign: 'right',
                                fontVariantNumeric: 'tabular-nums',
                              }}
                            >
                              {share}%
                            </TableCell>
                          </TableRow>
                        );
                      })}
                    </TableBody>
                  </Table>
                  </div>
                </div>
              </figure>
            )}
          </CardContent>
        </Card>
        <Card style={{ flex: '1 1 240px', minWidth: 0 }}>
          <CardHeader>
            <div>
              <CardTitle>Today</CardTitle>
              <CardDescription>
                {metric === 'sessions' ? 'Sessions vs daily average' : 'Words vs daily average'} · {todaySummary}
              </CardDescription>
            </div>
          </CardHeader>
          <CardContent>
            {todayRatio == null ? (
              <div className="chart-empty">
                No baseline yet. Press your hotkey to dictate.
              </div>
            ) : (
              <figure style={{ margin: 0 }}>
                {/* Single-scalar progress ring as plain SVG: a charting
                    library's scales buy nothing for one value, and a static
                    ring is reduced-motion compliant by construction (no
                    animation props to gate). Track paint mirrors the hero
                    grid treatment per theme. */}
                {/* The visible caption below carries the numbers, so the SVG is
                    hidden from assistive tech rather than repeating them. */}
                <svg
                  viewBox="0 0 120 120"
                  aria-hidden="true"
                  style={{ width: '100%', maxWidth: 180, height: 'auto', display: 'block', margin: '0 auto' }}
                >
                  <circle
                    cx={60}
                    cy={60}
                    r={52}
                    fill="none"
                    stroke="var(--color-border-structural)"
                    strokeWidth={12}
                  />
                  <circle
                    cx={60}
                    cy={60}
                    r={52}
                    fill="none"
                    stroke={duo.b}
                    strokeWidth={12}
                    strokeLinecap="round"
                    strokeDasharray={2 * Math.PI * 52}
                    strokeDashoffset={2 * Math.PI * 52 * (1 - ringSweep)}
                    transform="rotate(-90 60 60)"
                  />
                  <text
                    x={60}
                    y={60}
                    textAnchor="middle"
                    dominantBaseline="central"
                    style={{ fill: 'var(--color-on-surface)' }}
                    fontSize={22}
                    fontWeight={600}
                  >
                    {todayRatio.toFixed(1)}×
                  </text>
                  <text
                    x={60}
                    y={80}
                    textAnchor="middle"
                    style={{ fill: 'var(--color-on-surface-variant)' }}
                    fontSize={11}
                  >
                    of avg
                  </text>
                </svg>
                <div
                  style={{
                    marginTop: 8,
                    textAlign: 'center',
                    fontSize: 12,
                    color: 'var(--color-on-surface-variant)',
                    fontVariantNumeric: 'tabular-nums',
                  }}
                >
                  {todayValue.toLocaleString()} {weekdayUnit(todayValue)} today · {trailAvg.toFixed(1)} avg · last {trailDays}d
                </div>
              </figure>
            )}
          </CardContent>
        </Card>
        </div>
      </div>
    </section>
  );
}
