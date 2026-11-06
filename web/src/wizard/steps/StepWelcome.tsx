import { FluenceSymbol, FluenceWordmark } from '@/components/fluence/FluenceLogo';
// Step 1: Welcome. OOBE-minimal: brand mark, title, one-line promise, the
// three things Fluence does, Get Started. The mark is the brand master
// symbol (design/brand/master/fluence-symbol.svg) reproduced verbatim —
// never redrawn. The feature rows use typographic numerals, not icon glyphs:
// no lucide, no hand-rolled SVG, no emoji — just hairlines and type.
export function StepWelcome() {
  return (
    <>
      <div className="fluence-logo-container">
        <FluenceSymbol
          size={72}
          gradientId="fluence-brand-wizard"
          className="fluence-logo-svg"
        />
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
          gap: '8px',
        }}
      >
        Welcome to <FluenceWordmark fontSize="inherit" />
      </h1>
      <p className="step-desc">
        Your AI-powered voice typing assistant for Windows. Speak naturally, and
        Fluence transcribes your voice into any application, instantly.
      </p>
    </>
  );
}
