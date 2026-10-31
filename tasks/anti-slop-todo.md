# Todo: Anti-Slop Professional Polish (dark-only)

Plan: `tasks/anti-slop-plan.md`. Branch: `ui/anti-slop-professional-polish`. Do NOT merge to main without permission.

## Slice 1: Token drift fix + dark lock doc
- [x] Define `--color-surface-sunken` + `--color-border-subtle` in design-tokens from existing values
- [x] Header comment documents dark-only lock + accent/radius/type locks
- [x] Verification: typecheck + web build green; grep shows definitions exist

## Slice 2: Wordmark cleanup
- [x] REVERTED by owner (logo frozen, Allura reserved for logo only) — no wordmark change ships
- [x] `fluenceTranscribe` step-title string kept as `Fluence Transcribe` (announcer text, not logo — revert on request)

## Slice 3: Wizard + progress de-gradient
- [x] Step dot active + progress fills go flat cyan, no glow shadow
- [x] Applies to src/css/wizard.css, web wizard.css, ui.css progress blocks
- [x] Verification: build green; wizard 6 steps render; motion math unchanged

## Slice 4: Dashboard chart de-glow + About copy
- [x] Chart uses muted amethyst-cyan duo per owner (desaturated, no glow), React + vanilla in sync
- [x] About copy plain function, no poetry; keep IDs and updater logic
- [x] Verification: dashboard renders; tooltip intact; no em dashes

## Checkpoint: Complete
- [x] Each slice verified individually
- [x] Single branch, commits per slice, no merge to main
- [x] Ready for owner testing

## Light mode (settings shell only, dark default, overlay/wizard untouched)
- [x] Slice 1: theme plumbing (`lib/theme.ts`, pre-render apply, sidebar toggle, palette cmd, Ctrl+Shift+L)
- [x] Slice 2: `[data-theme="light"]` token block (zinc surfaces, deepened teal, AA measured)
- [x] Slice 3: additive light overrides (global/settings/ui/app css; dark rules untouched)
- [x] Slice 4: theme-aware dashboard duo (deepened light stops, themed legend color)
- [x] Slice 5: gates — typecheck+build green, zero backend files, all pairs AA (measured 3.99-17.72)
