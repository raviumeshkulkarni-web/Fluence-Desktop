import type { SVGProps } from 'react';

export interface FluenceLogoProps extends SVGProps<SVGSVGElement> {
  size?: number;
  /** Deprecated: the canonical mark uses per-capsule solid brand colors, no gradient. */
  gradientId?: string;
}

/**
 * Canonical Fluence brand mark - verbatim vector reproduction of the master
 * symbol (design/brand/master/fluence-symbol.svg), which is mathematically
 * rebuilt from the original 1024px artwork: 33 round-capped arc capsules on
 * three concentric rings plus the core dot, each with its native span,
 * thickness and brand color from the artwork. Individual capsule geometry
 * renders razor-sharp at every density; never substitute the old dashed
 * specks. Keep in sync with the master SVG.
 */
export function FluenceSymbol({
  size = 32,
  className,
  style,
  ...rest
}: FluenceLogoProps) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 32 32"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      shapeRendering="geometricPrecision"
      role="img"
      aria-label="Fluence"
      className={className}
      style={{ flexShrink: 0, ...style }}
      {...rest}
    >
      <g strokeLinecap="round" fill="none">
        {/* Inner ring capsules */}
        <path d="M 20.125 18.748 A 4.9562 4.9562 0 0 1 18.632 20.2" stroke="#1B87FA" strokeWidth={1.5} />
        <path d="M 16.867 20.88 A 4.9562 4.9562 0 0 1 15.065 20.867" stroke="#4A55FA" strokeWidth={1.5} />
        <path d="M 13.328 20.174 A 4.9562 4.9562 0 0 1 11.873 18.744" stroke="#603BF9" strokeWidth={1.5} />
        <path d="M 11.152 17.029 A 4.9562 4.9562 0 0 1 11.124 15.111" stroke="#6135F9" strokeWidth={1.5} />
        <path d="M 11.802 13.365 A 4.9562 4.9562 0 0 1 13.27 11.863" stroke="#6E3BFA" strokeWidth={1.5} />
        <path d="M 15.005 11.145 A 4.9562 4.9562 0 0 1 16.935 11.133" stroke="#6744F8" strokeWidth={1.5} />
        <path d="M 18.661 11.819 A 4.9562 4.9562 0 0 1 20.184 13.343" stroke="#3468F8" strokeWidth={1.5} />
        <path d="M 20.871 15.086 A 4.9562 4.9562 0 0 1 20.847 17.033" stroke="#0E8EFA" strokeWidth={1.5} />
        {/* Middle ring capsules */}
        <path d="M 23.853 19.35 A 8.5375 8.5375 0 0 1 22.823 21.132" stroke="#01B0F8" strokeWidth={1.609375} />
        <path d="M 21.227 22.751 A 8.5375 8.5375 0 0 1 19.315 23.867" stroke="#0E9BF8" strokeWidth={1.609375} />
        <path d="M 17.114 24.464 A 8.5375 8.5375 0 0 1 14.753 24.446" stroke="#4261FA" strokeWidth={1.609375} />
        <path d="M 12.616 23.838 A 8.5375 8.5375 0 0 1 10.732 22.718" stroke="#5F41F9" strokeWidth={1.609375} />
        <path d="M 9.146 21.09 A 8.5375 8.5375 0 0 1 8.144 19.343" stroke="#623BF9" strokeWidth={1.609375} />
        <path d="M 7.536 17.114 A 8.5375 8.5375 0 0 1 7.532 14.915" stroke="#6337FA" strokeWidth={1.609375} />
        <path d="M 8.121 12.712 A 8.5375 8.5375 0 0 1 9.119 10.946" stroke="#6939F9" strokeWidth={1.609375} />
        <path d="M 10.767 9.254 A 8.5375 8.5375 0 0 1 12.473 8.225" stroke="#7042F9" strokeWidth={1.609375} />
        <path d="M 14.701 7.562 A 8.5375 8.5375 0 0 1 17.21 7.549" stroke="#6757F8" strokeWidth={1.609375} />
        <path d="M 19.411 8.174 A 8.5375 8.5375 0 0 1 21.168 9.204" stroke="#3E73F8" strokeWidth={1.609375} />
        <path d="M 22.827 10.874 A 8.5375 8.5375 0 0 1 23.867 12.685" stroke="#0F93F8" strokeWidth={1.609375} />
        <path d="M 24.481 15.019 A 8.5375 8.5375 0 0 1 24.463 17.122" stroke="#01ABF8" strokeWidth={1.609375} />
        {/* Outer ring capsules */}
        <path d="M 27.134 19.694 A 11.7312 11.7312 0 0 1 25.772 22.491" stroke="#00C5F6" strokeWidth={1.640625} />
        <path d="M 24.111 24.475 A 11.7312 11.7312 0 0 1 21.787 26.205" stroke="#01B9F7" strokeWidth={1.640625} />
        <path d="M 19.409 27.225 A 11.7312 11.7312 0 0 1 17.187 27.671" stroke="#2388F9" strokeWidth={1.640625} />
        <path d="M 14.579 27.645 A 11.7312 11.7312 0 0 1 12.454 27.183" stroke="#514DFA" strokeWidth={1.640625} />
        <path d="M 10.107 26.144 A 11.7312 11.7312 0 0 1 7.815 24.404" stroke="#6542FA" strokeWidth={1.640625} />
        <path d="M 6.211 22.465 A 11.7312 11.7312 0 0 1 4.862 19.685" stroke="#7540F9" strokeWidth={1.640625} />
        <path d="M 4.323 17.123 A 11.7312 11.7312 0 0 1 4.449 13.954" stroke="#8041F8" strokeWidth={1.640625} />
        <path d="M 5.267 11.265 A 11.7312 11.7312 0 0 1 6.882 8.618" stroke="#8E48FA" strokeWidth={1.640625} />
        <path d="M 8.86 6.692 A 11.7312 11.7312 0 0 1 11.453 5.186" stroke="#9359F9" strokeWidth={1.640625} />
        <path d="M 14.085 4.426 A 11.7312 11.7312 0 0 1 17.884 4.421" stroke="#827BF8" strokeWidth={1.640625} />
        <path d="M 20.462 5.151 A 11.7312 11.7312 0 0 1 23.189 6.73" stroke="#459AF7" strokeWidth={1.640625} />
        <path d="M 25.124 8.626 A 11.7312 11.7312 0 0 1 26.737 11.274" stroke="#11ADF8" strokeWidth={1.640625} />
        <path d="M 27.557 13.984 A 11.7312 11.7312 0 0 1 27.676 17.133" stroke="#01C1F7" strokeWidth={1.640625} />
      </g>
      <circle cx="16" cy="16" r="1.4688" fill="#644AF9" />
    </svg>
  );
}

export interface FluenceWordmarkProps {
  fontSize?: number | string;
  className?: string;
}

/**
 * Canonical Fluence wordmark: "flu" (white/on-surface) + "ence" (cyan #0BD6E3).
 * Rendered using the official display typography (Sora / Hanken Grotesk).
 * "ence" (the last 4 letters) remains permanently constant (#0BD6E3) across
 * all themes and accent presets (owner brand rule).
 */
export function FluenceWordmark({
  fontSize = 22,
  className,
}: FluenceWordmarkProps) {
  return (
    <span
      className={className ?? 'logo-text'}
      style={{
        fontFamily: "var(--font-display, 'Sora', system-ui, sans-serif)",
        fontWeight: 600,
        fontSize,
        letterSpacing: '-0.02em',
        display: 'inline-flex',
        alignItems: 'baseline',
        color: 'var(--color-on-surface, #FFFFFF)',
      }}
    >
      flu<span style={{ color: 'var(--color-logo-cyan, #0BD6E3)' }}>ence</span>
    </span>
  );
}
