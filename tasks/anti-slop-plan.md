# Anti-Slop Professional Polish — Plan (dark-only locked)

Branch: `ui/anti-slop-professional-polish`. Never merge to `main` without explicit permission.
Backend frozen: `src-tauri/**`, capabilities, IPC names/shapes untouched.
Theme decision (owner): dark only for now. No light mode work. Document dark-only as intentional.

## Design read
Windows desktop product UI (settings app + recording overlay + first-run wizard) for daily voice typists, quiet studio hardware language, custom Tauri dark tokens + Windows 11 expectations.

Dials (product UI adaptation): VARIANCE 3 / MOTION 3 / DENSITY 6.

## Locks
- Accent: `--color-brand-cyan` `#0BD6E3` is the single functional accent. White stays for primary buttons and selected text. Amethyst `#8B45D8` stays ONLY inside the logo mark gradient. No other gradient washes.
- Radius rule: cards 12, inputs/selects 8, buttons 8, badges/pills 999, checkboxes 0 (square hardware), toggles pill by nature, dialogs 12, tooltips 3-4. No new radii.
- Type: Sora display + Hanken Grotesk body + Geist Mono labels/numbers. Allura removed everywhere. One wordmark: `Fluence Transcribe`.
- Icons: lucide-react stays (project already depends on it — allowed override). No new hand-rolled icon paths. Titlebar min/max keep geometric rects (OS convention), close becomes real X with tray explanation.

## Slices (each leaves tree working)
1. Token drift fix + dark lock doc. Define missing `--color-surface-sunken`, `--color-border-subtle` from existing tokens. Zero visual change except fixing transparent fallback bugs. Verify: typecheck + web build.
2. Wordmark cleanup. Remove Allura spans in `src/index.html` x2, `Sidebar.tsx`, `AboutPage.tsx`, `ui.css .logo-tagline`. Unify `fluenceTranscribe` camelCase to `Fluence Transcribe`. Keep logo rings gradient (only allowed gradient). Verify: routes render, no font reference left.
3. Wizard + progress de-gradient. Step dots active, progress fill, wizard progress track go flat cyan. Keep motion math. Verify: 6-step walkthrough renders, reduced motion intact.
4. Dashboard chart de-glow + About copy tighten. Remove `feDropShadow` glow filter and gradient stroke glow, flat cyan stroke, flat low-alpha fill. About marketing poetry replaced with plain function copy. Verify: dashboard renders, chart tooltip intact.

## Verification per slice
- `npm run typecheck --prefix web` green
- `npm run build --prefix web` green (writes src/dist, gitignored — build only to verify, do not commit dist)
- Grep: no `Allura`, no `surface-sunken` undefined, no new `—` em dashes in copy
- Manual `npm run dev` spot check of touched route only

## Risks
- Vanilla `src/` + React `web/src` dual implementation can drift. Mitigation: mirror wordmark/copy in both, note owner file per slice.
- `src/dist/` is gitignored build output. Never commit it.
- Allura woff2 stays in fonts/ until follow-up cleanup approved (removing binary asset + @font-face is separate decision).

## Done gate
All slices green, branch pushed (or local committed if no remote push approved), no merge to main, summary for owner testing.
