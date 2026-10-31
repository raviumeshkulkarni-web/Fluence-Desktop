// Step 2: features showcase. The full capability list gets its own screen
// so the welcome stays a quiet mark + promise. Same numeral-and-hairline
// language as before — no icon glyphs of any kind — set a touch larger here
// since this screen is the list's home.
const FEATURES = [
  { n: '01', title: 'Universal Voice Typing', desc: 'Works in any app: VS Code, Chrome, Word' },
  { n: '02', title: 'AI Agent Mode', desc: 'Long press to edit, rewrite, or command' },
  { n: '03', title: 'Secure & Private', desc: 'API keys stay in Windows Credential Manager' },
  { n: '04', title: 'Private Cloud Sync', desc: 'Securely stored in your own Google Drive, not on Fluence servers' },
  { n: '05', title: 'Custom Dictionary', desc: 'Names and jargon, spelled your way every time' },
  { n: '06', title: 'AI Cleanup', desc: 'Filler words, repeats, and typos removed' },
  { n: '07', title: 'Custom Snippets', desc: 'Say a shortcut, paste a full template' },
  { n: '08', title: 'Online + Offline, No Lock-In', desc: 'Cloud speed or on-device privacy with any API key' },
] as const;

export function StepFeatures() {
  return (
    <>
      <h1 className="step-title" tabIndex={-1}>
        What Fluence Can Do
      </h1>
      <p className="step-desc">
        A few highlights. Everything here works from your first recording.
      </p>
      <ol className="wiz-welcome-list wiz-features-showcase" aria-label="Fluence features">
        {FEATURES.map((f) => (
          <li key={f.n} className="wiz-welcome-row">
            <span className="wiz-welcome-num" aria-hidden="true">
              {f.n}
            </span>
            <span className="wiz-welcome-text">
              <span className="wiz-welcome-title">{f.title}</span>
              <span className="wiz-welcome-desc">{f.desc}</span>
            </span>
          </li>
        ))}
      </ol>
    </>
  );
}
