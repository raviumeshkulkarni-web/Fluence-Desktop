# Implementation Plan: UI Consistency Program (React main window)

## Overview
Job 1 (vanilla → React migration, 8 routes) is done. This program builds out the
full shared shadcn-Path-B component kit and migrates every remaining native
control to it, so the app reads as one professional-grade product: identical
dropdowns, checkboxes, inputs, dialogs, menus, motion, focus, and density on
every route. Behavior and IPC are frozen — this is presentation-only, except the
approved web-only Radix dependency additions.

End goal: every interactive nuance (dropdowns, animations, focus, hover,
density, states) is consistent across the whole UI/UX lifecycle of the app.
Overlay (`overlay.html`) stays vanilla — out of scope, per the standing freeze
boundary. The first-run wizard (`wizard.html`) IS in scope: converted to React
in Phase 4 against the same kit, keeping its 6-step flow and behavior identical.

## Architecture Decisions
- **Incumbent Fluence tokens win.** The ui-ux-pro-max `--design-system` pass
  (variance 3 / motion 3 / density 8, "Fluence") recommended teal/orange + Fira
  fonts — REJECTED. Refinement preserves the incumbent identity. Adopted from it:
  subtle motion tier (150–300ms, exit ≤250ms, `prefers-reduced-motion` guard),
  dense dashboard spacing (8–32px scale), cursor/hover/focus discipline.
- **shadcn Path B, no Tailwind/preflight.** New primitives copy the existing kit
  pattern: Radix anatomy + classes defined in `web/src/ui.css`, every value
  referencing a frozen vanilla token from `src/css/`. Nothing drifts.
- **No react-hook-form, no GSAP.** Routes keep controlled IPC-backed state
  (General's shell Esc/Ctrl+S semantics stay intact); motion is CSS transitions
  only. A shared labeled `Field` wrapper gives form consistency without RHF.
- **New Radix deps (web-only, APPROVED — pending final plan sign-off):**
  `@radix-ui/react-select`, `-dialog`, `-checkbox`, `-switch`, `-separator`,
  `-tooltip`, `-label`, `-progress`, `-radio-group`. Zero backend impact;
  `web/package.json` + lockfile only.
- **Vertical slices per route.** Each route task is independently verifiable
  (typecheck + build + preview that route) and leaves the tree working.
- **Avatar:** `components/ui/avatar.tsx` is currently unused — Phase 3 decides
  use-it (e.g. provider icons in Providers) or delete-it. No dead kit ships.

## Contracts / Guardrails (unchanged)
- Backend freeze: `src-tauri/**`, capabilities, IPC names/shapes untouched.
- `web/` builds to `src/dist/` (`base:'./'`, `target:es2020`); bundle < 900kB raw.
- `src/dist/`, `web/node_modules/`, logs gitignored. No commit unless instructed.
- AGENTS.md: caches/logs stay in project dir; review `capabilities/default.json`
  if adding frontend-accessible commands (not expected — no new commands).

## Task List

### Phase 1: Foundation (shared primitives + motion/focus tokens)
- [ ] Task 1: Install Radix packages + lockfile
- [ ] Task 2: `field/label/input` (+ textarea parity) primitives
- [ ] Task 3: `checkbox` + `switch` primitives
- [ ] Task 4: `select` primitive (Trigger/Value/Content/Item, full structure)
- [ ] Task 5: `dialog` primitive + shared `ConfirmDialog` (replaces
      `window.confirm` ×2)
- [ ] Task 6: `separator` + `tooltip` primitives
- [ ] Task 6b: `progress` + `radio-group` primitives (needed by wizard Phase 4)
- [ ] Task 7: Motion/focus tokens in `ui.css` (transition scale, focus-visible
      ring, reduced-motion guard, cursor-pointer rule)

### Checkpoint: Foundation
- [ ] `npm run typecheck` + `npm run web:build` green, bundle < 900kB raw
- [ ] Static-serve preview: each primitive renders in isolation (scratch check),
      keyboard Tab reaches all of them, focus ring visible
- [ ] Human review before route slices begin

### Phase 2: Route slices (natives → kit, behavior identical)
- [x] Task 8: General — 7 selects + 4 toggle-switches → kit Select/Switch + Field
      (verified: exactly 7 selects, 4 hardware toggles; no text inputs on General)
- [x] Task 9: Providers — 2 selects + 4 inputs + confirm-delete → ConfirmDialog
      (verified: 2 selects exist — plan's "3" was stale; engine picker is a
      semantic radiogroup, not a select)
- [x] Task 10: Dictionary — 2 toggle-switches + 2 text inputs + select-all/row
      checkboxes → kit Switch/Input/Checkbox (tri-state now declarative)
- [x] Task 11: History — clear-history → ConfirmDialog, search input → kit Input
- [x] Task 12: Snippets — toggle-switch + 2 text inputs → kit Switch/Input
- [x] Task 13: Sync — toggle-switch → Switch (verified: NO email input exists on
      SyncPage — plan "email input" was stale; buttons already kit Button)
- [x] Task 14: Dashboard + About — verified already kit; only Dashboard stroke
      token fix (`--color-sessions` undefined → frozen `--color-brand-cyan`)

### Checkpoint: Routes
- [ ] Typecheck + build green, bundle < 900kB raw, after EACH route task
- [ ] Preview each route: pixel-compare against pre-slice screenshot for
      layout; only control chrome may change
- [ ] No `window.confirm`, no bare `<select>`/checkbox/input left in routes
      (grep-verified)

### Phase 3: Consistency sweep + hardening
- [x] Task 15: Menu/button uniformity — all action buttons kit `Button`; Dashboard
      KPI menus = kit `DropdownMenu` (only dropdowns in app); History right-click
      menu stays custom (no ContextMenu primitive; cursor-anchored) but paints on
      dropdown tokens; General/History ghost reset/row buttons unified to kit
- [x] Task 16: Loading/empty/error audit — first-load `Skeleton` replaces empty
      flash on Dictionary/Snippets/History; About progress → kit `Progress`;
      providers/dashboard/toasts already disciplined
- [x] Task 17: Icon discipline — 6 inline SVGs → lucide (Search/Mic/X/BookOpen/
      Lightbulb/Zap), strokeWidth consistent, toast ✓ glyphs stay text
- [x] Task 18: Type hierarchy — audit-only: exactly one `<h1>` per route (×8),
      correct h2 counts, no skipped levels, zero `<h3>`
- [x] Task 19: Hover/focus/keyboard audit — `ui-focus-ring` added to General
      hotkey displays ×2 + Providers offline-model cards; rest already covered
- [x] Task 20: Avatar use-or-delete — **DELETE** decision (no user-identity
      surface; Providers icons already brand-accurate). avatar.tsx removed;
      `.avatar*` ui.css block + radix-avatar dep removed by lead
- [x] Task 21: Narrow-width + reduced-motion — wrap-only overflow guards on
      `.setting-row`/`.input-with-btn`/add-rows at 800px minimum; reduced-motion
      verified (frozen guard + gated AnimatedStat/Area)

### Checkpoint: Complete
- [ ] All acceptance criteria met; full-route preview pass recorded
- [ ] Bundle < 900kB raw / gzip reported
- [ ] Ready for human review; no commit (standing rule)

### Phase 4: Wizard window (first-run, `wizard.html` → React)
The 6-step setup flow (Welcome → Connect API Key → Set Hotkey → Overlay
Position → Test Setup → All Set), 561-line vanilla `wizard.js` + `wizard.css`,
migrated to React against the same kit. Behavior and IPC are frozen; only the
presentation layer changes. Needs `Progress` + `RadioGroup` primitives from
Phase 1 (Task 6b); mounts a new React entry point per window.

- [x] Task 22: Wizard React entry (web/wizard.html → web/src/wizard/main.tsx),
      vite multi-page input {main, wizard}, src/wizard.html meta-refresh shim
      (frozen URL resolves at app root; shim hands off to dist/wizard.html),
      IPC wrappers for wizard's 13 pre-existing commands, zero new commands
- [x] Task 23: Steps 1–2 (Welcome, Connect API Key) — Field/Input/Button kit,
      kit Select ×2, brand SVGs verbatim, Progress step indicator
- [x] Task 24: Step 3 (Set Hotkey) — wizard-local HotkeyRecorder (duplicates,
      not imports, General's builder — vanilla wizard omits Meta; separate
      window must not drag @/ipc/general), RadioGroup mode
- [x] Task 25: Steps 4–5 (Overlay Position, Test Setup) — RadioGroup
      position/mode, orb states, verbatim copy, role=status result
- [x] Task 26: Step 6 (All Set) + step-dots + keyboard nav + transitions;
      announcer + title-focus; reduced-motion via frozen global guard

### Checkpoint: Wizard
- [ ] Typecheck + build green; bundle < 900kB raw (wizard is a separate entry;
      its chunk measured independently)
- [ ] Full 6-step walkthrough in `npm run dev`: welcome → API key → hotkey →
      overlay → test → done, keyboard-reachable, focus returned per step
- [ ] No bare `<select>`/checkbox/input, no `window.confirm`, no emoji icons
      (grep-verified across wizard route too)
- [ ] Pixel-compare each step against pre-migration screenshots; only control
      chrome may change

## Risks and Mitigations
| Risk | Impact | Mitigation |
|---|---|---|
| Radix Select/Dialog behavior differs subtly from native (type-ahead, Esc, focus return) | Med | Keep vanilla keyboard contract: Esc closes, focus returns to trigger; verify per route in preview |
| Controlled-state rewiring breaks General shell shortcuts | Med | No state-shape changes; primitives are controlled with same value/onChange props |
| Bundle creep from 9 Radix pkgs + recharts | Low | Per-task bundle check; Radix is tree-shaken per-component; 900kB ceiling enforced; wizard is a separate entry chunk |
| Dialog focus-trap vs Tauri window focus | Low | Radix Dialog standard trap; verify dictation hotkeys unaffected in preview |
| Wizard shares no IPC with main window (separate entry) | Med | Reuse existing command wrappers only; no new commands; verify full 6-step flow in dev |
| Scope creep into overlay | Low | Overlay stays vanilla; any request to include it is a new plan |

## Open Questions
- Confirm new Radix deps approved with plan sign-off (assumed yes pending review).
- Confirm no commit at program end (standing rule) — final report only.
- Minimum Tauri window size for Task 21 (measure from `tauri.conf.json` at build time).
- Overlay stays vanilla (confirmed out of scope); wizard now IN scope per Phase 4.

## Sign-off
- **Scope:** main window 8 routes + first-run wizard converted to React kit;
  overlay stays vanilla. Human-approved (main + wizard in scope, overlay out).
- **Feedback applied:** Task 9 → 3 selects + 4 inputs; Task 13 → email input +
  checkbox + button; Task 17 → toast `✓` glyphs stay text (rule targets control
  icons); Progress/RadioGroup added to kit for Phase 4.
