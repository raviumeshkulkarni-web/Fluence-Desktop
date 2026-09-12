# Todo: Anti-Slop Professional Polish (dark-only)

Plan: `tasks/anti-slop-plan.md`. Branch: `ui/anti-slop-professional-polish`. Do NOT merge to main without permission.

## Slice 1: Token drift fix + dark lock doc
- [ ] Define `--color-surface-sunken` + `--color-border-subtle` in design-tokens from existing values
- [ ] Header comment documents dark-only lock + accent/radius/type locks
- [ ] Verification: typecheck + web build green; grep shows definitions exist

## Slice 2: Wordmark cleanup
- [ ] Remove Allura spans (src/index.html x2, Sidebar, AboutPage, ui.css tagline → sans)
- [ ] Unify `fluenceTranscribe` to `Fluence Transcribe`
- [ ] Verification: grep zero `Allura`; routes render; IDs unchanged

## Slice 3: Wizard + progress de-gradient
- [ ] Step dot active + progress fills go flat cyan, no glow shadow
- [ ] Applies to src/css/wizard.css, web wizard.css, ui.css progress blocks
- [ ] Verification: build green; wizard 6 steps render; motion math unchanged

## Slice 4: Dashboard chart de-glow + About copy
- [ ] Chart stroke flat cyan, fill flat low-alpha, remove glow filter use
- [ ] About copy plain function, no poetry; keep IDs and updater logic
- [ ] Verification: dashboard renders; tooltip intact; no em dashes

## Checkpoint: Complete
- [ ] Each slice verified individually
- [ ] Single branch, commits per slice, no merge to main
- [ ] Ready for owner testing
