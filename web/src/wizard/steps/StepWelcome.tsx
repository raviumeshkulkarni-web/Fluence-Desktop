import { Lock, Mic, Sparkles } from 'lucide-react';

// Step 1: Welcome. Brand logo SVG copied verbatim from vanilla
// src/wizard.html; feature glyphs are the lucide equivalents of the
// vanilla stroke icons (mic / sparkle / lock, 18px, 2px stroke).
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
          filter="url(#logo-glow)"
          aria-hidden="true"
        >
          <circle cx="16" cy="16" r="13" stroke="url(#logo-grad-wiz)" strokeWidth="2.6" strokeDasharray="1.8 3" strokeLinecap="round" />
          <circle cx="16" cy="16" r="9" stroke="url(#logo-grad-wiz)" strokeWidth="2.6" strokeDasharray="2 3" strokeLinecap="round" />
          <circle cx="16" cy="16" r="5" stroke="url(#logo-grad-wiz)" strokeWidth="2.6" strokeDasharray="2 2" strokeLinecap="round" />
          <circle cx="16" cy="16" r="2.2" fill="url(#logo-grad-wiz)" />
          <defs>
            <linearGradient id="logo-grad-wiz" x1="0" y1="0" x2="32" y2="32" gradientUnits="userSpaceOnUse">
              <stop offset="0%" stopColor="#8B45D8" />
              <stop offset="50%" stopColor="#8B45D8" />
              <stop offset="100%" stopColor="#0BD6E3" />
            </linearGradient>
            <filter id="logo-glow" x="-50%" y="-50%" width="200%" height="200%">
              <feGaussianBlur in="SourceGraphic" stdDeviation="1.2" result="blur1" />
              <feGaussianBlur in="SourceGraphic" stdDeviation="3.5" result="blur2" />
              <feMerge>
                <feMergeNode in="blur2" />
                <feMergeNode in="blur1" />
              </feMerge>
            </filter>
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
      <div className="wizard-feature-list">
        <div className="wizard-feature-card">
          <div className="wizard-feature-icon" aria-hidden="true">
            <Mic size={18} strokeWidth={2} />
          </div>
          <div className="wizard-feature-info">
            <div className="wizard-feature-title">Universal Voice Typing</div>
            <div className="wizard-feature-desc">Works in any app: VS Code, Chrome, Word</div>
          </div>
        </div>
        <div className="wizard-feature-card">
          <div className="wizard-feature-icon" aria-hidden="true">
            <Sparkles size={18} strokeWidth={2} />
          </div>
          <div className="wizard-feature-info">
            <div className="wizard-feature-title">AI Agent Mode</div>
            <div className="wizard-feature-desc">Long press to edit, rewrite, or command</div>
          </div>
        </div>
        <div className="wizard-feature-card">
          <div className="wizard-feature-icon" aria-hidden="true">
            <Lock size={18} strokeWidth={2} style={{ color: 'var(--color-success)' }} />
          </div>
          <div className="wizard-feature-info">
            <div className="wizard-feature-title">Secure &amp; Private</div>
            <div className="wizard-feature-desc">API keys stored in Windows Credential Manager</div>
          </div>
        </div>
      </div>
    </>
  );
}
