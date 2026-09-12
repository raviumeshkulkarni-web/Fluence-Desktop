import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import type { MouseEvent as ReactMouseEvent } from 'react';
import { Area, AreaChart, CartesianGrid, XAxis, YAxis } from 'recharts';
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
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { Skeleton } from '@/components/ui/skeleton';
import {
  ChartContainer,
  type ChartConfig,
} from '@/components/ui/chart';
import { toast } from '@/components/fluence/Toasts';
import {
  getAccountActivity,
  type DailyBucket,
} from '@/ipc/dashboard';
import { copyText, subscribeHistoryUpdated } from '@/ipc/history';
import { subscribeSyncStatus } from '@/ipc/sync';

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

interface RangePoint {
  label: string;
  count: number;
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
      const weeks = new Map<number, number>();
      for (const b of buckets) {
        const monday = b.day_start_ms - ((new Date(b.day_start_ms).getUTCDay() + 6) % 7) * DAY_MS;
        weeks.set(monday, (weeks.get(monday) ?? 0) + b.sessions);
      }
      return [...weeks.entries()]
        .sort((a, b) => a[0] - b[0])
        .map(([monday, count]) => ({
          label: new Date(monday).toLocaleDateString(undefined, {
            month: 'short',
            day: 'numeric',
          }),
          count,
        }));
    }
    const months = new Map<number, number>();
    for (const b of buckets) {
      const d = new Date(b.day_start_ms);
      const key = d.getUTCFullYear() * 12 + d.getUTCMonth();
      months.set(key, (months.get(key) ?? 0) + b.sessions);
    }
    return [...months.entries()]
      .sort((a, b) => a[0] - b[0])
      .map(([key, count]) => ({
        label: new Date(Date.UTC(Math.floor(key / 12), key % 12, 1)).toLocaleDateString(
          undefined,
          { month: 'short', year: 'numeric' },
        ),
        count,
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
  const byDay = new Map(buckets.map((b) => [b.day_start_ms, b.sessions]));
  const today = utcDayStart(Date.now());
  return Array.from({ length: n }, (_, k) => {
    const dayMs = today - (n - 1 - k) * DAY_MS;
    const d = new Date(dayMs);
    return {
      label: label(d),
      count: byDay.get(dayMs) ?? 0,
    };
  });
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

const chartConfig: ChartConfig = {
  sessions: { label: 'Sessions', color: '#0BD6E3' },
};

// Module cache (stale-then-reload across remounts, no re-skeleton).
let cachedBuckets: DailyBucket[] | null = null;
let dashboardLoaded = false;
let skeletonCleared = false;

export function DashboardPage() {
  const [buckets, setBuckets] = useState<DailyBucket[]>(cachedBuckets ?? []);
  const [loaded, setLoaded] = useState(dashboardLoaded);
  const [skeleton, setSkeleton] = useState(!skeletonCleared);
  const [range, setRange] = useState<Range>('7d');
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
        foot: `${totals.sessions.toLocaleString()} sessions · ${totals.scope}`,
      },
      {
        id: 'stat-time-saved',
        title: 'Typing Time Saved',
        value: formatDurationMs((totals.words / 40) * 60000),
        foot: `at ~40 WPM · ${totals.scope}`,
      },
      {
        id: 'stat-dictation-time',
        title: 'Dictation Time',
        value: formatDurationMs(totals.durationMs),
        foot: `${spokenLabel(totals.durationMs)} · ${totals.scope}`,
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
  const rangeTotal = points.reduce((a, p) => a + p.count, 0);
  // Recharts does not auto-size the axis: 4-digit counts overflow a 32px
  // gutter and the SVG viewport clips their leading digit.
  const yWidth = points.some((p) => p.count >= 1000) ? 44 : 32;

  // Android-parity hover chip: anchored to the active data point, clamped
  // horizontally into the plot, flipping below the point when there is no
  // room above. Driven by native mouse position (no recharts event API),
  // so the geometry is explicit: left gutter = yWidth, right/top margins
  // mirror the AreaChart margin prop, X labels occupy ~30px at the bottom.
  const hostRef = useRef<HTMLDivElement>(null);
  const chipRef = useRef<HTMLDivElement>(null);
  const [hoverIdx, setHoverIdx] = useState<number | null>(null);
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
    const frac = (e.clientX - rect.left - plotL) / Math.max(1, plotR - plotL);
    const idx = Math.round(frac * (points.length - 1));
    setHoverIdx(Math.min(points.length - 1, Math.max(0, idx)));
  };
  const hovered = hoverIdx != null && points[hoverIdx] != null ? points[hoverIdx] : null;
  const hostW = hostRef.current?.offsetWidth ?? 0;
  const hostH = hostRef.current?.offsetHeight ?? 0;
  const yMax = Math.max(1, ...points.map((p) => p.count));
  const hoverGeom = (() => {
    if (hovered == null || hoverIdx == null || hostW <= 0 || hostH <= 0) return null;
    const plotL = yWidth;
    const plotR = hostW - 8;
    const plotT = 8;
    const plotB = hostH - 30;
    const frac = points.length < 2 ? 0.5 : hoverIdx / (points.length - 1);
    const dotX = plotL + frac * Math.max(0, plotR - plotL);
    const dotY = plotT + (1 - hovered.count / yMax) * Math.max(0, plotB - plotT);
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
                    <TrendBadge buckets={buckets} range={range} />
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
              <CardDescription>Transcription sessions</CardDescription>
            </div>
            <Tabs value={range} onValueChange={(v) => setRange(v as Range)}>
              <TabsList aria-label="Activity range">
                <TabsTrigger value="7d">Last 7 days</TabsTrigger>
                <TabsTrigger value="30d">Last 30 days</TabsTrigger>
                <TabsTrigger value="90d">Last 90 days</TabsTrigger>
                <TabsTrigger value="all">All time</TabsTrigger>
              </TabsList>
            </Tabs>
          </CardHeader>
          <CardContent>
            {loaded && rangeTotal === 0 ? (
              <div className="chart-empty" id="chart-empty">
                No activity in this range yet — press your hotkey to dictate.
              </div>
            ) : (
              <div
                ref={hostRef}
                className="chart-hover-host"
                onMouseMove={onPlotMove}
                onMouseLeave={() => setHoverIdx(null)}
              >
              <ChartContainer config={chartConfig} height="100%">
                <AreaChart data={points} margin={{ top: 8, right: 8, bottom: 0, left: 0 }}>
                  <defs>
                    <linearGradient id="dashAreaGrad" x1="0" y1="0" x2="0" y2="1">
                      <stop offset="0%" stopColor="#0BD6E3" stopOpacity={0.28} />
                      <stop offset="50%" stopColor="#0BD6E3" stopOpacity={0.12} />
                      <stop offset="100%" stopColor="#0BD6E3" stopOpacity={0.04} />
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
                    tickLine={false}
                    axisLine={false}
                    tick={{ fill: '#A0A0A0', fontSize: 12 }}
                  />
                  <Area
                    type="monotone"
                    dataKey="count"
                    name="sessions"
                    stroke="var(--color-brand-cyan)"
                    strokeWidth={2}
                    fill="url(#dashAreaGrad)"
                    dot={false}
                    isAnimationActive={!reduced}
                    animationDuration={225}
                  />
                </AreaChart>
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
                      {hovered.count} session{hovered.count === 1 ? '' : 's'} ·
                    </span>
                    <span className="chart-tooltip-label">{hovered.label}</span>
                  </div>
                </div>
              )}
              </div>
            )}
          </CardContent>
        </Card>
      </div>
    </section>
  );
}
