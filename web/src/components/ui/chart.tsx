import * as React from 'react';
import { ResponsiveContainer, Tooltip } from 'recharts';
import { cn } from '@/lib/cn';

// Minimal shadcn-style chart wrapper over recharts: typed config,
// themed container, token-styled tooltip. No Tailwind; paint from ui.css.
export type ChartConfig = {
  [key: string]: {
    label?: string;
    color?: string;
  };
};

function ChartContainer({
  config,
  className,
  height = 260,
  children,
}: {
  config: ChartConfig;
  className?: string;
  height?: number | `${number}%`;
  children: React.ReactElement;
}) {
  const vars = Object.fromEntries(
    Object.entries(config).map(([key, item]) => [`--color-${key}`, item.color ?? '']),
  ) as React.CSSProperties;
  return (
    <div className={cn('chart-container', className)} style={vars}>
      <ResponsiveContainer width="100%" height={height}>
        {children}
      </ResponsiveContainer>
    </div>
  );
}

const ChartTooltip = Tooltip;

interface TooltipPayloadEntry {
  value?: number | string;
  dataKey?: string | number;
  color?: string;
}

function ChartTooltipContent({
  active,
  payload,
  label,
  valueFormatter,
}: {
  active?: boolean;
  payload?: TooltipPayloadEntry[];
  label?: string | number;
  valueFormatter?: (value: number, label: string) => string;
}) {
  if (!active || !payload || payload.length === 0) return null;
  const entry = payload[0];
  const numeric = typeof entry.value === 'number' ? entry.value : 0;
  const text = valueFormatter
    ? valueFormatter(numeric, String(label ?? ''))
    : `${numeric} sessions on ${label ?? ''}`;
  return (
    <div className="chart-tooltip">
      <span className="chart-tooltip-value">{text}</span>
    </div>
  );
}

export { ChartContainer, ChartTooltip, ChartTooltipContent };
