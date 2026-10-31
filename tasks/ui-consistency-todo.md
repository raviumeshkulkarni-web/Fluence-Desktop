# Todo: UI Consistency Program (React main window)

Plan: `tasks/ui-consistency-plan.md`. Status: PHASE 1 COMPLETE — verified
2026-09-07, STOPPED at Checkpoint: Foundation per instructions. No Phase 2 work.
Scratch harness (`web/src/scratch.tsx` + temporary `App.tsx` hook) built,
verified, then DELETED; `App.tsx` diff confirmed empty; production `src/dist/`
rebuilt byte-identical (`index-XvfLbdj8.js`, 778.65kB raw).

## Phase 1: Foundation

## Task 1: Install Radix packages + lockfile
**Description:** Add the 7 approved Radix primitives to `web/` and install.
**Acceptance criteria:**
- [ ] `web/package.json` gains select/dialog/checkbox/switch/separator/tooltip/label
- [ ] `npm install` clean, lockfile updated, no other dependency drift
**Verification:**
- [ ] `npm run typecheck` green
- [ ] `npm run web:build` green, bundle < 900kB raw
**Dependencies:** Plan sign-off. **Files:** `web/package.json`, `web/package-lock.json`. **Scope:** S

## Task 2: field/label/input primitives
**Description:** Shared `Field` wrapper (label + hint + error slots) plus
token-styled `Input` covering text/search/number/password as used by routes.
**Acceptance criteria:**
- [ ] Controlled API mirrors native props (`value`, `onChange`, `disabled`, ids)
- [ ] Label association correct (`htmlFor`/`id`), error slot present
**Verification:**
- [ ] Typecheck + build green; scratch preview renders all variants
- [ ] Tab reaches inputs; focus ring visible
**Dependencies:** Task 1. **Files:** `web/src/components/ui/field.tsx`,
`web/src/components/ui/input.tsx`, `web/src/ui.css`. **Scope:** S

## Task 3: checkbox + switch primitives
**Description:** Token-styled Radix Checkbox + Switch with label-line pattern.
**Acceptance criteria:**
- [ ] Indeterminate/checked/unchecked + disabled states render from tokens
- [ ] Label click toggles; keyboard Space toggles
**Verification:** Typecheck + build green; scratch preview incl. keyboard.
**Dependencies:** Task 1. **Files:** `checkbox.tsx`, `switch.tsx`, `ui.css`. **Scope:** S

## Task 4: select primitive
**Description:** Full-structure Select (Trigger/Value/Content/Item) per shadcn
stack guidance — never a bare trigger, never native `<select>`.
**Acceptance criteria:**
- [ ] Placeholder/value/anatomy complete; long option lists scroll in viewport
- [ ] Type-ahead + Esc-close + focus-return behave like the native contract
**Verification:** Typecheck + build green; scratch preview incl. keyboard.
**Dependencies:** Task 1. **Files:** `select.tsx`, `ui.css`. **Scope:** S

## Task 5: dialog primitive + ConfirmDialog
**Description:** Token-styled Radix Dialog plus a shared destructive-confirm
dialog that replaces both `window.confirm` call sites.
**Acceptance criteria:**
- [ ] Focus trap + Esc-close + overlay click policy defined once
- [ ] ConfirmDialog takes title/body/confirm-label/danger-flag/onConfirm
**Verification:** Typecheck + build green; scratch preview incl. Esc + trap.
**Dependencies:** Task 1. **Files:** `dialog.tsx`, `ui.css`. **Scope:** S

## Task 6: separator + tooltip primitives
**Description:** Token HR/vertical separator + delay-tuned tooltip for icon
buttons and truncated labels.
**Acceptance criteria:**
- [ ] Separator visible in both themes; tooltip delay single-sourced
**Verification:** Typecheck + build green; scratch preview.
**Dependencies:** Task 1. **Files:** `separator.tsx`, `tooltip.tsx`, `ui.css`. **Scope:** XS

## Task 7: Motion/focus tokens
**Description:** Centralize transition scale (150–300ms), focus-visible ring,
cursor-pointer rule, and `prefers-reduced-motion` guard in `ui.css`.
**Acceptance criteria:**
- [ ] One duration/easing scale; all kit components reference it
- [ ] Reduced-motion disables non-essential transitions
**Verification:** Typecheck + build green; preview with reduced-motion on.
**Dependencies:** Tasks 2–6. **Files:** `web/src/ui.css`. **Scope:** S

## Checkpoint: Foundation
- [ ] Typecheck + build green, bundle < 900kB raw
- [ ] Scratch preview: every primitive keyboard-reachable with visible focus
- [ ] Human review before route slices

## Phase 2: Route slices (behavior identical)

## Task 8: General route
**Description:** Swap 7 selects + 4 checkboxes to kit with Field wrappers.
**Acceptance criteria:**
- [ ] Zero bare `<select>`/checkbox remain; settings persist identically
- [ ] Shell Esc/Ctrl+S semantics unchanged
**Verification:** Typecheck + build green, bundle check; preview pixel-compare.
**Dependencies:** Checkpoint Foundation. **Files:** `GeneralPage.tsx`. **Scope:** M

## Task 9: Providers route
**Description:** 2 selects + inputs to kit; delete → ConfirmDialog.
**Acceptance criteria:**
- [ ] All 16 IPC commands still wired; delete flow confirms via dialog
**Verification:** Typecheck + build green; preview incl. delete flow.
**Dependencies:** Checkpoint Foundation. **Files:** `ProvidersPage.tsx`. **Scope:** M

## Task 10: Dictionary route
**Description:** Checkboxes + inputs + Field wrappers; keep pairKey NUL rule.
**Acceptance criteria:**
- [ ] Auto-learn toggles behave identically; no state-shape change
**Verification:** Typecheck + build green; preview.
**Dependencies:** Checkpoint Foundation. **Files:** `DictionaryPage.tsx`. **Scope:** S

## Task 11: History route
**Description:** Clear-history → ConfirmDialog; search input → kit Input.
**Acceptance criteria:**
- [ ] Clear flow confirms via dialog; search/paging/groups unchanged
**Verification:** Typecheck + build green; preview incl. clear flow.
**Dependencies:** Checkpoint Foundation. **Files:** `HistoryPage.tsx`. **Scope:** S

## Task 12: Snippets route
**Description:** Checkboxes + inputs to kit.
**Acceptance criteria:** Toggle/add/edit flows identical.
**Verification:** Typecheck + build green; preview.
**Dependencies:** Checkpoint Foundation. **Files:** `SnippetsPage.tsx`. **Scope:** S

## Task 13: Sync route
**Description:** Checkbox + button consistency with kit.
**Acceptance criteria:** Sync flows identical; status subscription untouched.
**Verification:** Typecheck + build green; preview.
**Dependencies:** Checkpoint Foundation. **Files:** `SyncPage.tsx`. **Scope:** XS

## Task 14: Dashboard + About pass
**Description:** Align existing kit usage with new motion/focus/density tokens.
**Acceptance criteria:**
- [ ] No visual regression in KPIs/chart/tabs; feet/badges unchanged
**Verification:** Typecheck + build green; preview all 4 tabs.
**Dependencies:** Checkpoint Foundation. **Files:** `DashboardPage.tsx`, `AboutPage.tsx`. **Scope:** S

## Checkpoint: Routes
- [ ] Grep: no `window.confirm`, no bare `<select>`/checkbox/input in routes
- [ ] Bundle < 900kB raw; full-route preview pass

## Phase 3: Sweep

## Task 15: Menu uniformity
**Description:** One dropdown-menu pattern across History/Dashboard/providers.
**Acceptance criteria:** Same trigger affordance, placement, Esc, item density.
**Verification:** Preview each menu incl. keyboard. **Scope:** S

## Task 16: Loading/empty/error states
**Description:** Audit all routes for skeleton-vs-spinner discipline; no blank
or frozen screens during IPC waits.
**Acceptance criteria:** Every async view has loading + empty + error coverage.
**Verification:** Preview with slow/stubbed IPC where feasible. **Scope:** M

## Task 17: Icon discipline
**Description:** lucide-only, single stroke/size token scale, no emoji icons.
**Acceptance criteria:** Grep shows no non-lucide icon usage in routes.
**Verification:** Preview icon-heavy routes (Providers, History). **Scope:** S

## Task 18: Type hierarchy
**Description:** h1 once per page; section/subsection levels never skipped.
**Acceptance criteria:** Heading outline correct on all 8 routes.
**Verification:** DOM outline check in preview. **Scope:** S

## Task 19: Hover/focus/keyboard audit
**Description:** Visible focus everywhere, 150–300ms transitions, Esc closes
all overlays, focus returns to triggers.
**Acceptance criteria:** Full Tab-walk of every route with zero focus loss.
**Verification:** Manual keyboard pass in preview. **Scope:** M

## Task 20: Avatar use-or-delete
**Description:** Use `avatar.tsx` somewhere real or delete it — no dead kit.
**Acceptance criteria:** Imported by a route or file removed.
**Verification:** Typecheck + build green. **Scope:** XS

## Task 21: Narrow-width + reduced-motion
**Description:** Check Tauri minimum window width; verify no clipped controls;
full reduced-motion pass.
**Acceptance criteria:** No horizontal clipping at minimum width; motion off
under reduced-motion with no layout shift.
**Verification:** Preview at min width + reduced-motion. **Scope:** S

## Checkpoint: Complete
- [ ] All boxes above checked; bundle numbers reported; preview evidence saved
- [ ] Ready for human review; no commit
