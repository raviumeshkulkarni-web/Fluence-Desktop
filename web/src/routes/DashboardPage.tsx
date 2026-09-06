import {
  useEffect,
  useMemo,
  useRef,
  useState,
} from 'react';
import { getAccountStats, getWeeklyActivity } from '@/ipc/dashboard';
import { subscribeHistoryUpdated } from '@/ipc/history';

const CHART_HEIGHT = 244;
const PADDING_X = 14;
const BASELINE_Y = CHART_HEIGHT - 14;
const TOP_Y = 20;
const AVAIL_HEIGHT = BASELINE_Y - TOP_Y;
const DAY_NAMES = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];
const DOT_COLORS = ['#8B45D8', '#854BD9', '#7855DC', '#5F69E0', '#3E95E2', '#1DBEE3', '#0BD6E3'];

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
    <div className="stat-value" id={id}>
      {display}
    </div>
  );
}

interface ChartPoint {
  x: number;
  y: number;
  count: number;
}

// Vanilla renderWeeklyAreaChart geometry, verbatim: measured canvas width
// clamped to [280, 1400] (640 fallback), Catmull-Rom to Bezier spline.
function chartPoints(dayCounts: number[], width: number): ChartPoint[] {
  const maxCount = Math.max(...dayCounts, 1);
  const stepX = (width - PADDING_X * 2) / 6;
  return dayCounts.map((c, i) => {
    const x = PADDING_X + i * stepX;
    const factor = Math.min(1, Math.max(0, c / maxCount));
    return { x, y: c > 0 ? BASELINE_Y - factor * AVAIL_HEIGHT : BASELINE_Y, count: c };
  });
}

function curvePath(points: ChartPoint[]): string {
  let d = 'M ' + points[0].x.toFixed(1) + ' ' + points[0].y.toFixed(1);
  for (let i = 0; i < points.length - 1; i++) {
    const p0 = points[i === 0 ? 0 : i - 1];
    const p1 = points[i];
    const p2 = points[i + 1];
    const p3 = points[i + 2 < points.length ? i + 2 : i + 1];
    const cp1x = p1.x + (p2.x - p0.x) / 6;
    const cp1y = Math.min(BASELINE_Y, Math.max(TOP_Y - 6, p1.y + (p2.y - p0.y) / 6));
    const cp2x = p2.x - (p3.x - p1.x) / 6;
    const cp2y = Math.min(BASELINE_Y, Math.max(TOP_Y - 6, p2.y - (p3.y - p1.y) / 6));
    d +=
      ' C ' + cp1x.toFixed(1) + ' ' + cp1y.toFixed(1) + ', ' +
      cp2x.toFixed(1) + ' ' + cp2y.toFixed(1) + ', ' +
      p2.x.toFixed(1) + ' ' + p2.y.toFixed(1);
  }
  return d;
}

function prefersReducedMotion(): boolean {
  return (
    typeof window.matchMedia === 'function' &&
    window.matchMedia('(prefers-reduced-motion: reduce)').matches
  );
}

// Curve + area with vanilla's fade-in (replayed on every data change,
// skipped under reduced motion).
function ChartPaths({ dCurve, dArea }: { dCurve: string; dArea: string }) {
  const reduced = useMemo(prefersReducedMotion, []);
  const [faded, setFaded] = useState(reduced);
  useEffect(() => {
    if (reduced) return;
    const raf = requestAnimationFrame(() => setFaded(true));
    return () => {
      cancelAnimationFrame(raf);
      setFaded(false);
    };
  }, [dCurve, dArea, reduced]);
  const curveStyle = reduced
    ? { opacity: 1 }
    : { opacity: faded ? 1 : 0, transition: 'opacity 0.35s ease' };
  const areaStyle = reduced
    ? { opacity: 1 }
    : { opacity: faded ? 1 : 0, transition: 'opacity 0.45s ease' };
  return (
    <>
      <path id="weekly-area-path" d={dArea} fill="url(#area-gradient)" style={areaStyle} />
      <path
        id="weekly-curve-path"
        d={dCurve}
        fill="none"
        stroke="url(#curve-stroke-grad)"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
        filter="url(#curve-glow)"
        vectorEffect="non-scaling-stroke"
        style={curveStyle}
      />
    </>
  );
}

interface StatTexts {
  totalWords: string;
  timeSaved: string;
  dictation: string;
  monthly: string;
  totalWordsSub: string;
  timeSavedSub: string;
  dictationSub: string;
  monthlySub: string;
  headerCount: string;
  rangeLabel: string;
}

const STATIC_STATS: StatTexts = {
  totalWords: '0',
  timeSaved: '0m',
  dictation: '0m',
  monthly: '0m',
  totalWordsSub: 'to date',
  timeSavedSub: 'to date',
  dictationSub: 'to date',
  monthlySub: 'in last 30 days',
  headerCount: 'Loading…',
  rangeLabel: 'Last 7 days',
};

// Vanilla keeps dashboard DOM across navigation (no re-skeleton, no
// re-animation, stale data until reload). These module caches reproduce
// exactly that across React remounts.
let cachedStats: StatTexts | null = null;
let cachedCounts: number[] | null = null;
let cachedWeekStartMs = 0;
let skeletonCleared = false;

// Faithful port of the vanilla Dashboard surface (#page-dashboard +
// loadDashboardStats/renderWeeklyAreaChart/setupChartResize, boot skeleton
// handling). Same DOM ids/classes, same copy, same derivations, same
// silent console-error failure path (stats keep their defaults).
export function DashboardPage() {
  const [stats, setStats] = useState<StatTexts>(cachedStats ?? STATIC_STATS);
  const [dayCounts, setDayCounts] = useState<number[]>(
    cachedCounts ?? [0, 0, 0, 0, 0, 0, 0],
  );
  const [weekStartMs, setWeekStartMs] = useState(cachedWeekStartMs);
  const [loaded, setLoaded] = useState(cachedCounts !== null);
  const [skeleton, setSkeleton] = useState(!skeletonCleared);
  const [chartWidth, setChartWidth] = useState(640);
  const [tipIdx, setTipIdx] = useState<number | null>(null);
  const canvasColRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const bStats = await getAccountStats();
        if (cancelled) return;
        const totalWords = bStats.total_words;
        const weeklyWords = bStats.weekly_words;
        const weeklyDurationMs = bStats.weekly_duration_ms;
        const monthlyWords = bStats.monthly_words;

        const weeklyHoursSaved = weeklyWords / 40 / 60;
        const weeklyDictationHours = weeklyDurationMs / 3600000;
        const weeklyWordsLabel =
          weeklyWords >= 1000
            ? (weeklyWords / 1000).toFixed(1) + 'K'
            : weeklyWords.toLocaleString();
        const weeklySavedLabel =
          weeklyHoursSaved >= 1
            ? weeklyHoursSaved.toFixed(1) + 'h saved'
            : Math.round(weeklyHoursSaved * 60) + 'm saved';
        const weeklyDictLabel =
          weeklyDictationHours >= 1
            ? weeklyDictationHours.toFixed(1) + 'h spoken'
            : Math.round(weeklyDictationHours * 60) + 'm spoken';

        let rangeLabel = 'Last 7 days';
        try {
          const start = new Date(bStats.week_start_ms);
          const end = new Date(bStats.week_start_ms + 6 * 86400000);
          const fmt = new Intl.DateTimeFormat('en-US', {
            month: 'short',
            day: 'numeric',
          });
          rangeLabel = fmt.format(start) + ' – ' + fmt.format(end);
        } catch {
          /* range label is decorative */
        }

        const next: StatTexts = {
          totalWords: formatTotalWords(totalWords),
          timeSaved: formatDurationMs((totalWords / 40) * 60000),
          dictation: formatDurationMs(bStats.total_duration_ms),
          monthly: formatDurationMs((monthlyWords / 40 / 60) * 3600000),
          totalWordsSub:
            '+' +
            (weeklyWords >= 1000
              ? (weeklyWords / 1000).toFixed(1) + 'K'
              : String(weeklyWords)) +
            ' this week',
          timeSavedSub: 'at ~40 WPM · to date',
          dictationSub: weeklyDictLabel + ' · last 7 days',
          monthlySub:
            (monthlyWords >= 1000
              ? (monthlyWords / 1000).toFixed(1) + 'K'
              : String(monthlyWords)) + ' words · last 30 days',
          headerCount:
            weeklyWordsLabel + ' words · ' + weeklySavedLabel + ' · ' + weeklyDictLabel,
          rangeLabel,
        };
        cachedStats = next;
        setStats(next);

        const monday = new Date(bStats.week_start_ms);
        let timestamps: string[];
        if (bStats.source === 'account' && Array.isArray(bStats.weekly_timestamps)) {
          timestamps = bStats.weekly_timestamps;
        } else {
          timestamps = await getWeeklyActivity(monday.toISOString());
          if (cancelled) return;
        }
        const counts = [0, 0, 0, 0, 0, 0, 0];
        timestamps.forEach((ts) => {
          const d = new Date(ts);
          const dow = d.getUTCDay();
          counts[dow === 0 ? 6 : dow - 1]++;
        });
        cachedCounts = counts;
        cachedWeekStartMs = bStats.week_start_ms;
        setDayCounts(counts);
        setWeekStartMs(bStats.week_start_ms);
        setLoaded(true);
      } catch (err) {
        console.error('Failed to load dashboard stats:', err);
      }
    };
    void load().finally(() => {
      skeletonCleared = true;
      if (!cancelled) setSkeleton(false);
    });
    let unlisten: (() => void) | undefined;
    void subscribeHistoryUpdated(() => void load()).then((u) => {
      unlisten = u;
    });
    const onFocus = () => void load();
    window.addEventListener('focus', onFocus);
    return () => {
      cancelled = true;
      unlisten?.();
      window.removeEventListener('focus', onFocus);
    };
  }, []);

  // ResizeObserver re-renders identical data at the new width — no
  // backend call (vanilla setupChartResize, 120ms debounce).
  useEffect(() => {
    const el = canvasColRef.current;
    if (!el || typeof ResizeObserver === 'undefined') return;
    const measure = () =>
      setChartWidth((prev) => {
        const next = Math.max(280, Math.min(1400, el.clientWidth || 640));
        return prev === next ? prev : next;
      });
    measure();
    let t: number | null = null;
    const obs = new ResizeObserver(() => {
      if (t !== null) window.clearTimeout(t);
      t = window.setTimeout(measure, 120);
    });
    obs.observe(el);
    return () => {
      if (t !== null) window.clearTimeout(t);
      obs.disconnect();
    };
  }, []);

  const now = Date.now();
  const total = dayCounts.reduce((a, b) => a + b, 0);
  let peakIdx = 0;
  dayCounts.forEach((c, i) => {
    if (c > dayCounts[peakIdx]) peakIdx = i;
  });
  const maxCount = Math.max(...dayCounts, 1);
  const activeDays = dayCounts.filter((c) => c > 0).length;
  const avg = total > 0 ? total / 7 : 0;
  let todayIdx = -1;
  if (weekStartMs) {
    todayIdx = Math.floor((now - weekStartMs) / 86400000);
    if (todayIdx < 0 || todayIdx > 6) todayIdx = -1;
  }

  const points = useMemo(
    () => chartPoints(loaded ? dayCounts : [0, 0, 0, 0, 0, 0, 0], chartWidth),
    [loaded, dayCounts, chartWidth],
  );
  const dCurve = useMemo(() => curvePath(points), [points]);
  const dArea = useMemo(
    () =>
      dCurve +
      ' L ' + points[6].x.toFixed(1) + ' ' + BASELINE_Y +
      ' L ' + points[0].x.toFixed(1) + ' ' + BASELINE_Y + ' Z',
    [dCurve, points],
  );

  const showChart = loaded;
  const tip = tipIdx !== null ? { count: dayCounts[tipIdx], x: points[tipIdx].x, y: points[tipIdx].y } : null;

  return (
    <section className="page active" id="page-dashboard">
      <div className="page-header">
        <h1 className="page-title" tabIndex={-1}>Dashboard</h1>
        <p className="page-subtitle">Your transcription activity at a glance</p>
      </div>

      <div className="stats-grid">
        <div className={`stat-card${skeleton ? ' skeleton' : ''}`}>
          <div className="stat-eyebrow">Lifetime</div>
          <AnimatedStat id="stat-total-words" value={stats.totalWords} />
          <div className="stat-label">Words Transcribed</div>
          <div className="stat-sub" id="stat-total-words-sub">{stats.totalWordsSub}</div>
        </div>
        <div className={`stat-card${skeleton ? ' skeleton' : ''}`}>
          <div className="stat-eyebrow">Estimate · 40 WPM</div>
          <AnimatedStat id="stat-time-saved" value={stats.timeSaved} />
          <div className="stat-label">Typing Time Saved</div>
          <div className="stat-sub" id="stat-time-saved-sub">{stats.timeSavedSub}</div>
        </div>
        <div className={`stat-card${skeleton ? ' skeleton' : ''}`}>
          <div className="stat-eyebrow">Recorded</div>
          <AnimatedStat id="stat-dictation-time" value={stats.dictation} />
          <div className="stat-label">Dictation Time</div>
          <div className="stat-sub" id="stat-dictation-sub">{stats.dictationSub}</div>
        </div>
        <div className={`stat-card${skeleton ? ' skeleton' : ''}`}>
          <div className="stat-eyebrow">Last 30 days</div>
          <AnimatedStat id="stat-monthly-saved" value={stats.monthly} />
          <div className="stat-label">30-Day Time Saved</div>
          <div className="stat-sub" id="stat-monthly-sub">{stats.monthlySub}</div>
        </div>
      </div>

      <div className="settings-section dashboard-chart-card">
        <div className="settings-section-header dashboard-chart-header">
          <div className="dashboard-chart-titles">
            <h2>Weekly Activity</h2>
            <p className="chart-range" id="chart-range-label">{stats.rangeLabel}</p>
          </div>
          <span id="chart-header-count" className="chart-summary">{stats.headerCount}</span>
        </div>
        <div className="dashboard-chart-body" id="weekly-chart">
          <div className="chart-y-axis" aria-hidden="true">
            <span className="axis-title-y">Transcriptions</span>
          </div>

          <div className="chart-canvas-col" id="chart-canvas-col" ref={canvasColRef}>
            <svg
              id="weekly-area-svg"
              role="img"
              aria-label={
                loaded
                  ? 'Weekly transcription activity: ' + total + ' transcriptions this week. ' +
                    dayCounts.map((c, i) => DAY_NAMES[i] + ' ' + c).join(', ')
                  : 'Weekly transcription activity area chart'
              }
              viewBox={`0 0 ${chartWidth} ${CHART_HEIGHT}`}
            >
              <defs>
                <linearGradient id="area-gradient" x1="0" y1="0" x2="0" y2="1">
                  <stop offset="0%" stopColor="#0BD6E3" stopOpacity="0.45" />
                  <stop offset="50%" stopColor="#8B45D8" stopOpacity="0.30" />
                  <stop offset="100%" stopColor="#8B45D8" stopOpacity="0.14" />
                </linearGradient>
                <linearGradient
                  id="curve-stroke-grad"
                  x1="0"
                  y1="0"
                  x2={String(chartWidth)}
                  y2="0"
                  gradientUnits="userSpaceOnUse"
                >
                  <stop offset="0%" stopColor="#8B45D8" />
                  <stop offset="50%" stopColor="#6C5CE7" />
                  <stop offset="100%" stopColor="#0BD6E3" />
                </linearGradient>
                <filter id="curve-glow" x="-10%" y="-20%" width="120%" height="140%">
                  <feDropShadow dx="0" dy="2" stdDeviation="4" floodColor="#0BD6E3" floodOpacity="0.35" />
                </filter>
              </defs>
              {showChart && (
                <>
                  <g id="weekly-chart-grid">
                    {[TOP_Y, TOP_Y + AVAIL_HEIGHT / 2, BASELINE_Y].map((y, i) => (
                      <line
                        key={y}
                        x1={PADDING_X}
                        x2={chartWidth - PADDING_X}
                        y1={y.toFixed(1)}
                        y2={y.toFixed(1)}
                        stroke={i === 2 ? 'rgba(255,255,255,0.14)' : 'rgba(255,255,255,0.06)'}
                        strokeWidth="1"
                        strokeDasharray={i === 2 ? undefined : '3 3'}
                      />
                    ))}
                  </g>
                  <ChartPaths dCurve={dCurve} dArea={dArea} />
                  <g id="weekly-chart-points">
                    {points.map((pt, idx) => {
                      const isPeak = total > 0 && pt.count === maxCount && pt.count > 0;
                      return (
                        <g key={idx}>
                          <circle
                            cx={pt.x.toFixed(1)}
                            cy={pt.y.toFixed(1)}
                            r={pt.count > 0 ? (isPeak ? '7' : '6') : '3'}
                            fill={DOT_COLORS[idx]}
                            opacity={pt.count > 0 ? '0.22' : '0.05'}
                          />
                          <circle
                            cx={pt.x.toFixed(1)}
                            cy={pt.y.toFixed(1)}
                            r={pt.count > 0 ? (isPeak ? '4' : '3.4') : '2.2'}
                            fill={isPeak ? '#FFFFFF' : '#0D0D0D'}
                            stroke={DOT_COLORS[idx]}
                            strokeWidth="2"
                            className="chart-data-dot"
                            tabIndex={0}
                            role="img"
                            aria-label={DAY_NAMES[idx] + ': ' + pt.count + ' transcriptions'}
                            onMouseEnter={() => setTipIdx(idx)}
                            onMouseLeave={() => setTipIdx(null)}
                            onFocus={() => setTipIdx(idx)}
                            onBlur={() => setTipIdx(null)}
                          />
                          {pt.count > 0 && (
                            <text
                              x={pt.x.toFixed(1)}
                              y={Math.max(10, pt.y - 12).toFixed(1)}
                              className={'chart-value-label' + (isPeak ? ' is-peak' : '')}
                            >
                              {String(pt.count)}
                            </text>
                          )}
                        </g>
                      );
                    })}
                  </g>
                  <g id="weekly-chart-labels"></g>
                </>
              )}
            </svg>
            <div className="chart-day-labels" id="chart-day-labels">
              {DAY_NAMES.map((day, i) => (
                <div
                  key={day}
                  className={
                    'chart-day-cell' +
                    (loaded && i === todayIdx ? ' is-today' : '') +
                    (loaded && total > 0 && i === peakIdx && dayCounts[i] > 0 ? ' is-peak' : '')
                  }
                  data-day={i}
                >
                  <span className="day-name">{day}</span>
                </div>
              ))}
            </div>
          </div>

          <div className="chart-tooltip" id="chart-tooltip" hidden={tip === null}>
            {tip !== null && (
              <>
                <strong>{tip.count}</strong> · {DAY_NAMES[tipIdx ?? 0]}
              </>
            )}
          </div>
          <div className="chart-empty" id="chart-empty" hidden={total !== 0 || !loaded}>
            No activity this week yet — press your hotkey to dictate.
          </div>
        </div>

        <div className="insights-grid">
          <div className="insight-card">
            <div className="insight-eyebrow">Most active day</div>
            <div className="insight-value" id="insight-best-day">
              {loaded && total > 0 ? DAY_NAMES[peakIdx] : '–'}
            </div>
            <div className="insight-sub" id="insight-best-day-sub">
              {loaded && total > 0 ? dayCounts[peakIdx] + ' transcriptions' : loaded ? 'no activity yet' : 'this week'}
            </div>
          </div>
          <div className="insight-card">
            <div className="insight-eyebrow">Daily average</div>
            <div className="insight-value" id="insight-daily-avg">
              {loaded ? (total > 0 ? (avg >= 10 ? String(Math.round(avg)) : avg.toFixed(1)) : '0') : '–'}
            </div>
            <div className="insight-sub">transcriptions / day</div>
          </div>
          <div className="insight-card">
            <div className="insight-eyebrow">Active days</div>
            <div className="insight-value" id="insight-active-days">
              {loaded ? String(activeDays) : '–'}
            </div>
            <div className="insight-sub" id="insight-active-sub">
              of 7 days{loaded && activeDays === 7 ? ' · perfect week' : ''}
            </div>
          </div>
          <div className="insight-card">
            <div className="insight-eyebrow">Today</div>
            <div className="insight-value" id="insight-today">
              {loaded ? (todayIdx >= 0 ? String(dayCounts[todayIdx]) : '–') : '–'}
            </div>
            <div className="insight-sub" id="insight-today-sub">
              {loaded
                ? todayIdx >= 0
                  ? DAY_NAMES[todayIdx] + ' · so far'
                  : 'outside this week'
                : 'so far'}
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}
