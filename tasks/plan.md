# Implementation Plan: Windows U-path Auto Learn Repair

## Overview
Make the Windows post-injection Auto Learn watcher (U-path) reliably capture user
corrections: adopt the Android filter rules (no 0.40 similarity gate), harden the
UIA watcher against transient failures, and make every session exit visible in
release logs. Human Accept workflow unchanged; case-only pairs are allowed into
suggestions.json (approved deviation from Android); P-path and all
transcription/audio code untouched.

## Architecture Decisions
- Filter contract = Android `WordLcsExtractor.isValidCorrection`, minus Android's
  repo-level case-only drop (approved: case-only must reach the human gate).
- Safety posture unchanged: confirmed password/read-only still aborts; only
  transient *errors* are retried/tolerated.
- No new dependencies, no schema changes, no UI changes, no new log sink
  (env_logger + documented RUST_LOG=info).

## Task List
- [ ] Task 1: Android-style `is_valid_correction` + test updates
- [ ] Task 2: DROPPED by approver (no `suggestion.rs` change; `suggestion.rs` untouched)
- [ ] Task 3: UIA resilience (retry helper, transient tolerance, initial retry,
        injected-text containment check + `InjectedTextNotObserved` reason)
- [ ] Task 4: Ungate session logging to `info!`
- [ ] Checkpoint: focused unit tests + `npm run check` + `npm run clippy` green;
        stop before manual Notepad verification (owner tests it).

## Risks and Mitigations
| Risk | Impact | Mitigation |
|---|---|---|
| Flipped test asserts look like regressions | Low | Asserts rewritten to the approved layered contract |
| More permissive filter → noisier suggestions | Med | Human Accept (kept) absorbs it; frequency sorting ranks repeats |
| Retries slightly delay session exit | Low | Bounded (4x250ms init, 3x transient); timeout backstop unchanged |

## Open Questions (resolved)
- Case-only into suggestions? YES (approver decision, overrides Android parity).

## Known residual (NOT a regression — do not "fix" without schema work)
- Key-scoped deny gap: the delete→dismiss linkage is pair-scoped and only
  fires when a suggestion row exists. Deleting a never-pushed manually-added
  entry leaves zero trace (hard delete, no tombstone, no deny record), so a
  later ≥3x cross-day observation reads the spoken key as Free and may
  auto-accept it. Matches existing manual semantics. A true key-scoped deny
  needs new state (key-level tombstone table or key-only Dismissed slot).
  Documented in code at `dismiss_matching_suggestions` + `spoken_key_state`.
- Daily auto-accept cap REMOVED per product decision (tempo gate is the
  anti-burst control). `accepted_at` stamping retained for the dictionary
  provenance badge. No cap wording remains except the design note.
