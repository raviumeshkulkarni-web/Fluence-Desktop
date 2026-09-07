# Todo: Windows U-path Auto Learn Repair

## Task 1: Android-style `is_valid_correction`
- [x] Rewrite rules in `src-tauri/src/auto_learn/ui_automation.rs::is_valid_correction`
      (case-sensitive equality; both-sides-`<2`-chars reject; numeric-mix veto;
      remove Levenshtein gate and lowercased compares; drop now-unused import)
- [x] Update flipped in-file asserts; add numeric-mix / both-single-char tests
- [x] Update `src-tauri/src/auto_learn/correction_extractor.rs` test expects
      that change (`test_short_words_skipped`; capitalization test stays green
      via the >50% veto — verify, don't assume)
- [x] Verification: focused unit tests pass

## Task 3: UIA resilience
- [x] Retry helper for fallible bool props in `FocusedTextReader::new`
      (3x100ms; abort only on confirmed `Ok(true)`)
- [x] Transient `NoElement`/`NoValue` tolerance (3x) in `read_current_value`;
      immediate exit kept for FocusChanged/Secure/ReadOnly
- [x] Initial-capture retry (4x250ms) + injected-text containment check with new
      `ExitReason::InjectedTextNotObserved` in `monitor.rs`
- [x] Verification: unit tests pass; `cargo check` clean

## Task 4: Session logging
- [x] Ungate `log_session_result` from `debug_assertions`; raise to `info!`
      plus one `info!` exit-reason line per session
- [x] Verification: present in release-path code (`cargo check`); runtime
      check deferred to owner manual test with `RUST_LOG=info`

## Checkpoint: Complete
- [x] Focused `cargo test auto_learn` green
- [x] `npm run check` green
- [x] `npm run clippy` green
## Task 5 (added during live debugging, approved): suggestion save fix
- [x] `src-tauri/src/suggestion.rs::save_to_disk`: tolerant sync
      (`let _ =`, matches all other stores) — root cause of zero suggestions
- [x] Verification: suggestion unit tests (8) pass; check + clippy clean
## Task 6 (approved): settle-based extraction, no per-session cap
- [x] `src-tauri/src/auto_learn/monitor.rs`: settle gate (2 identical reads),
      final end-of-session diff on Timeout/FocusChanged only
- [x] Verification: auto_learn tests (84) pass; check + clippy clean
## Task 8 (approved): anchor-miss instrumentation (no behavior change)
- [x] `src-tauri/src/auto_learn/correction_extractor.rs`: info-level,
      metadata-only miss reasons (not-in-baseline / ambiguous repeat /
      context-broken)
- [x] Verification: auto_learn tests (88) pass; clippy clean
- [ ] STOP — soak testing collects the metric; tail-relaxation only if
      context-broken fires regularly
