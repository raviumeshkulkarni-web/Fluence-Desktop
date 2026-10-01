// Step 1: Welcome. OOBE-minimal: brand mark, title, one-line promise, the
// three things Fluence does, Get Started. The mark is the brand master
// symbol (design/brand/master/fluence-symbol.svg) reproduced verbatim —
// never redrawn. The feature rows use typographic numerals, not icon glyphs:
// no lucide, no hand-rolled SVG, no emoji — just hairlines and type.
export function StepWelcome() {
  return (
    <>
      <div className="fluence-logo-container">
        <svg
          className="fluence-logo-svg"
          width="72"
          height="72"
          viewBox="0 0 32 32"
          fill="none"
          xmlns="http://www.w3.org/2000/svg"
          role="img"
          aria-label="Fluence logo"
        >
          <circle cx="16" cy="16" r="13" stroke="url(#fluence-brand)" strokeWidth="1.8" strokeDasharray="1.5 3.5" strokeLinecap="round" />
          <circle cx="16" cy="16" r="9" stroke="url(#fluence-brand)" strokeWidth="1.8" strokeDasharray="1.8 3.5" strokeLinecap="round" />
          <circle cx="16" cy="16" r="5" stroke="url(#fluence-brand)" strokeWidth="1.8" strokeDasharray="2 3" strokeLinecap="round" />
          <circle cx="16" cy="16" r="1.5" fill="url(#fluence-brand)" />
          <defs>
            <linearGradient id="fluence-brand" x1="0" y1="0" x2="32" y2="32" gradientUnits="userSpaceOnUse">
              <stop offset="0%" stopColor="#8B45D8" />
              <stop offset="50%" stopColor="#8B45D8" />
              <stop offset="100%" stopColor="#0BD6E3" />
            </linearGradient>
          </defs>
        </svg>
      </div>
      <h1
        className="step-title"
        tabIndex={-1}
        style={{
          fontWeight: 600,
          letterSpacing: '-0.03em',
          display: 'inline-flex',
          alignItems: 'baseline',
          justifyContent: 'center',
        }}
      >
        Welcome to flu<span style={{ color: 'var(--color-brand-cyan)' }}>ence</span>
        <span
          style={{
            fontFamily: "'Allura',cursive",
            fontWeight: 400,
            fontSize: 'var(--text-label-lg)',
            marginLeft: 6,
            lineHeight: 1,
            color: 'var(--color-on-surface-variant)',
          }}
        >
          Transcribe
        </span>
      </h1>
      <p className="step-desc">
        Your AI-powered voice typing assistant for Windows. Speak naturally, and
        Fluence transcribes your voice into any application, instantly.
      </p>
    </>
  );
}
