# Implementation Plan: Windows Dictionary — Garbage Suggestion Pairings Fix (Correction Decision Core Learned From a Reference Design + Fluence Safety Extras)

**Status:** APPROVED v5 — CLOSED (2026-10-20). Round-4 audit REQUEST-CORRECTION
implemented and ACKed by the reviewer (second pass); F7 flake record-only;
implementation verified green; remaining item = owner soak only.

**Scope rule:** Dictionary/suggestion/auto-learn subsystem ONLY. Audio, DSP,
STT internals, polish flows, snippets runtime, UI components, Tauri command
surface, Android: untouched. Prior plan (`tasks/plan.md`) untouched.

---

## 0. Context and directives (2026-10-20)

- Symptom (owner screenshot): Suggestions tab shows nonsense pairs at 4x–8x:
  `stuck→agent(8x)`, `CS50→tabs(5x)`, `have→Type(5x)`, `release→agent(5x)`,
  `again→agent(5x)`, `in→add(4x)`, `Let→et(4x)`, `do→add(4x)`,
  `check→add(4x)`, `testing→ChatGPT(4x)`, `in→to(4x)`.
- Owner directive 1: learn from the working correction-learning design of the
  reference implementation (study location preserved verbatim:
  `D:\Working files\Review apps\Openwhisper for review\openwhispr`).
  Knowledge learned; no third-party source copied.
- Owner directive 2: exact same design, Rust, provable same behavior — no
  second guessing. Android is NOT a reference (shows zero suggestions after
  days of use; opposite failure; later batch; Windows-only plan).

---

## 1. Verified root cause

P-path (raw STT vs dictionary/snippet-corrected text, `extraction.rs`) is not
the observed source for 10/11 pairs: requires exactly 1 delete + 1 insert,
both ≥3 chars, `normalized_levenshtein ≥ 0.40` (extraction.rs:181). Nuance
(review round 1): `again→agent` sits at sim 0.400 = exactly at the P-path
boundary — P-path still cannot CREATE it (a created pair requires the pair to
already exist in the dictionary, in which case the store-side skip catches
it — so unlearned garbage cannot arrive via P-path; the sim boundary is
formality). P-path IS a rewrite source in one known out-of-scope bug (F5:
per-app polish override vs global-style extraction context,
workflow.rs:484-485) — acknowledged, not fixed here. Also possible per-take
P-path pollution source (Task 6): snippet trigger→expansion applications.

All observed garbage originates in the **U-path** (post-injection watcher):

### RC-1 — tie-diagonal pairing manufactures unrelated pairs (`correction_extractor.rs:197-228`)
DP tie ⇒ diagonal `(Some(orig), Some(edit))` pair even when words differ.
Reproduction shape (verified): injected
`[I, went, to, the, shunade, and, it, was, cool]`, user deletes
`and it was cool`, types `Good agent stuff` ⇒ OLD code pairs: `cool→stuff`,
`was→agent`, `it→Good` (tie-diagonal). PORTED outcome: exactly ONE
adjacency pair `cool→Good` (dist 2/4, sim 0.50) — which PASSES the gates ⇒
exactly ONE surviving candidate (boundary-word residual, owner-accepted
class; §5). Corrected in v4 per reviewer round-2; the reviewer's own "final
zero candidates" claim was wrong on this one point (rebutted, §8).

### RC-2 — pair filter passes everything (`ui_automation.rs:347-373`)
Rejects only empty / case-sensitive-identical / both-<2-chars / digit-vs-
nondigit. No similarity floor, no corrected-length floor.

### RC-3 — frequency inflation (`monitor.rs:386-427` + final diff `486-508`)
Every settled state re-diffs against the ORIGINAL injection; no per-session
pair memory; final diff can re-count again.

### RC-4 — store-level compounding (`suggestion.rs:196-201`)
`frequency += 1` on any re-observation; P-path batches lack intra-batch dedup.

### RC-5 (new in v3) — rewrite veto goes soft under B1 alignment
The old tie rule existed partly to push rewrites into the >50%-subs veto.
After the port (B1), a full rewrite collapses to ≤1 boundary pair; the
subs-count veto no longer fires (e.g. `we should release` → `replace the
plan`: 1 sub of 3 words = 33% < 50%). The pair gates alone then LEAK a
similar-looking boundary pair: `release→replace` has sim 0.571 (dist 3/7)
and would pass the 0.65 ratio (0.43 ≤ 0.65). In the reference design this
leak is
benign — a learned WORD in a flat list, no rewrite semantics. In Fluence a
learned pair is an ACTIVE REWRITE RULE (dictionary.rs `apply_corrections`).
This platform asymmetry is why this adaptation needs one Fluence safety
extra: an
LCS-coverage rewrite guard (Task 1, item 4).

---

## 2. Architecture decisions (reference-derived design + named safety extras)

- **Decision core = behavior-preserving adaptation** of the reference
  correction learner's algorithm
  into a NEW platform-neutral module
  `src-tauri/src/auto_learn/decision_core.rs` (2026-10-24 rename: module
  renamed at owner's direction so naming reflects learned knowledge, not a
  code port/copy), with a reference-anchor map
  header (reference ranges → Rust lines) and a conformance test suite whose
  cases mirror the reference's cases. The reference gate-skips —
  duplicate-pair → lowercase-identical → corrected-length → sim-floor —
  plus insertion-preferring tie-break (ref L106), adjacency-only pairing
  (ref L115-124), and >50% rewrite veto (ref L149-151) carried over as-is;
  the reference's
  word-list dict-skip is NOT carried over — replaced by pair-level store
  skip/dedup (D4).
- **Module placement constraint (review A5, verified by me):** the new module
  must be declared OUTSIDE the `#[cfg(target_os = "windows")]` block in
  `auto_learn/mod.rs` (alongside `extraction`, line 10 pattern) because
  `suggestion.rs` is not Windows-gated and will import the shared judge;
  `ui_automation.rs` is Windows-gated (mod.rs:12-19) — a purge importing from
  it breaks non-Windows builds.
- **Safety extras beyond reference parity (Fluence extras, each justified):**
  1. **LCS-coverage rewrite guard:** `if dp[m][n] * 2 < injected_words.len()
     → treat as rewrite → no candidates`. Pair-count-independent proxy for
     the old veto (validated on: full rewrites → vetoed; RC-1 (LCS 5, m 9)
     → not vetoed (v4-correction: the surviving `cool→Good` candidate is a
     boundary-word residual, NOT a "legit correction"; `we should release` case →
     LCS 0 → vetoed).
  2. **Numeric-mix/numeric policy** — decided in OQ-6 (default: exact parity,
     no exemption).
  3. **Case-only handling** — decided in OQ-5 (default: exact parity = skip).
- **Deliberate shell deviations (non-algorithmic):**
  D1. Fluence anchored-span (`extract_edited_span`) replaces the reference's
      looser `findEditedRegion`; strictly tighter containment, never widens.
  D2. Capture shell stays Fluence UIA (platform plumbing).
  D3. Storage/product shell stays (queue + human gate + auto-accept OFF).
      the reference writes into a flat word list with undo; adopting THAT
      means
      UI/command rewrites — violates isolation.
  D4. Adviser G (accepted, overrides my v2 union idea): keep Fluence's
      PAIR-level store skip (`should_skip_new_candidate` via
      `learner::get_current_dictionary`), NOT the reference's corrected-word
      skip — pair-level is more correct for a spoken→corrected store.
- `strsim::normalized_levenshtein` (in-tree) ≡ `1 - dist/maxLen`; reference
  `dist/maxLen ≤ 0.65` ⇒ floor `sim ≥ 0.35`.

## 3. Explicitly untouched (blast radius)

- `audio.rs` (not read), `transcribe.rs` STT internals
- `workflow.rs`, `prompts.rs` (polish flow; F5 recorded as separate follow-up)
- `snippets.rs` runtime, `dictionary.rs` runtime (`apply_corrections`)
- `extraction.rs` (P-path flow untouched; NOTE: the store task touches
  `upsert_suggestions` which P-path also calls — frequency accounting
  behavior for P-path candidates changes ONLY if Task 5 (optional batch
  dedup) is kept; flow logic itself untouched)
- `snippets.rs` — Task 6 target (new additive helper; v4 correction: the
  v3 `learner.rs` skip-set fingerprint is DROPPED — the skip check lives in
  `suggestion.rs`'s upsert prelude, both branches guarded)
- Frontend `web/src/**`, Android, capabilities, no new Tauri commands.

---

## 4. Task list

### Task 1 — Decision-core module `decision_core.rs`
**Files:** NEW `src-tauri/src/auto_learn/decision_core.rs` (renamed
2026-10-24 at owner's direction — naming reflects learned knowledge, not a
code copy);
`correction_extractor.rs` becomes: anchor → tokenize (inside decision core)
→ decision core.
Old `find_substitutions` deleted or kept dead-commented _per reviewer round 2_
(prefer delete; its tests migrate).
**Port (JS line anchors):**
  1. `tokenize` (JS L28-33): split whitespace, strip edge non-`[\p{L}\p{N}_]`.
  2. `find_substitutions` (JS L83-127): full traceback; diagonal only on
     case-insensitive-equal words; ties prefer insertion side
     (`dp[i][j-1] >= dp[i-1][j]`); pairs ONLY from consecutive
     `[Some(orig),None] + [None,edit]` adjacency.
  3. Fluence extra: **LCS-coverage rewrite guard** — `2 * dp[m][n] < m` ⇒
     return [] BEFORE pairing (validated: protects against RC-5 leak).
  4. `extract_corrections(injected, edited_region, known_pair_keys)` (JS
     L137-177): early-outs identical; >50% subs veto retained (JS L151);
     skips in JS order: pair-known-in-dictionary → duplicate pair →
     lowercase-identical → `corrected.length < 3` →
     `_distribution > 0.65` (i.e. `sim < 0.35` reject).
  5. Export `is_valid_pair(spoken, corrected, known) -> bool` (JS L158-174
     as a judge, for extraction AND purge) + `known_pair_keys()` helper.
     (Reviewer: neutral module location — DONE via module placement.)
  6. Line-anchor header `// JS Lxx → Rust Lxx` and a module-top citation of
     the reference file path + commit-ish note "reference read 2026-10-20".
**Conformance + regression tests (transliterated, not re-derived):**
  - [ ] Full-rewrite leak test: `we should release` → `replace the plan`
        ⇒ ZERO candidates (guard catches pair `release→replace`, sim 0.571).
  - [ ] Rewrite with garbage boundary: `the quick brown fox` →
        `a slow red cat jumped` ⇒ zero (guard; previously relied on wrong
        mechanism — test states guard-based reason).
  - [ ] `test_simple_correction` logic equivalent via port:
        `shunade→Sinead` (sim 0.43 ≥ 0.35 ✓ len ✓), `Johnatan→Jonathan`,
        `definately→definitely` (0.9), `recognision→recognition`,
        `accomodate→accommodate`, `test_accept_boundary_similarity`
        (`abc→axc`).
  - [ ] Screenshot pairs (assert via the real judge, parametrized):
        stuck→agent(0.00), CS50→tabs(0.00), have→Type(0.25), release→agent
        (0.14), in→add(0.00), do→add(0.333 — tightest margin, documents the
        0.017 distance from the floor), check→add(0.00), testing→ChatGPT
        (<0.35), Let→et(len rule ONLY — dist 1/3, sim 0.667 passes; the
        length gate is load-bearing — no double-kill), in→to(0.00,
        len rule), **again→agent(0.400) ⇒ SURVIVES — explicit residual test
        + acceptance note (not purged; dismiss silences it).**
  - [ ] RC-1 shape (v4-corrected): port yields exactly ONE adjacency pair
        `cool→Good` (dist 2/4, sim 0.50) and ZERO old-style tie-pairs
        (`cool→stuff`/`was→agent`/`it→Good` must not occur). `cool→Good`
        PASSES the gates ⇒ assertion: exactly ONE surviving candidate
        (boundary-word residual, documented; NOT a zero-candidate claim).
  - [ ] Anchored-span tests (L318-385) stay green unmodified — they test the
        anchoring layer only.

### Task 2 — Retire old filter from the live U-path
**File:** `ui_automation.rs` — comment block L340-346 rewritten (Android
contract void; reference-citation). `is_valid_correction` deleted or
deprecated to call the port's `is_valid_pair`   _per reviewer round 2_; zero
live (non-test) importers after wiring. Numeric + case-only handling
RESOLVED (OQ-6 exact parity; OQ-5 skip) — decided in ONE place (the ported
judge).
**Acceptance criteria:**
  - [ ] `rg is_valid_correction` → no live U-path caller; no non-Windows
        import risk.
  - [ ] Complete test-flip ledger (v4, supersedes v3):
        `test_accept_dissimilar_words`, `test_accept_unrelated_words`,
        `test_accept_both_short_but_not_trivial` (ab→cd sim 0.0),
        `test_accept_short_corrected` (ab→... corrected len 2 — same reason),
        `test_accept_reported_case` (shwande→Sinead 0.286 — OQ-1, resolved),
        `test_allow_punctuation_only_difference` (dr.→dr — corrected len 2;
        the quotes variant stays green),
        `test_allow_case_only_difference` + `..._all_upper` (FLIP to
        expect-reject — OQ-5 SKIP; lowercase-identical gate; names verified
        at ui_automation.rs:433/440),
        `test_accept_number_to_number` (FLIP to expect-reject: 123→456
        sim 0.0 — OQ-6 exact parity; the old digit-mix veto is NOT ported),
        `test_reject_mixed_number_and_word` (stays green — gates reject at
        sim 0.0; no digit rule needed).
        Extractor side: `test_short_words_surface_for_human_review`
        (a→the sim 0.00 — rejects via ratio 1.0; flip; OQ-2).

### Task 3 — Session pair memory (anti-inflation)
**File:** `monitor.rs` (settle loop L386-427 + final diff L486-508).
**Spec (v3, per reviewer A8):** memory created inside
`run_monitoring_session` (NOT the long-lived monitor thread); key =
`dictionary::canonical_entry_key(spoken, corrected)`; NOT seeded from disk
(cross-session re-observation is the legitimate frequency signal);
applied to both settle-diff and final-diff saves; same-session repeat of an
identical pair is dropped (documented consequence: a same-correction-taken-
twice-in-one-dictation counts once — frequency accrues across dictations).
Pure helper or small state struct + tests:
  - [ ] two settles same pair ⇒ 1 saved; two settles different pairs ⇒ 2.
  - [ ] final diff after settle value capture ⇒ +0.
  - [ ] helper unit-tested; monitor serialization tests untouched.
**Residual (documented, from reviewer):** revisit same-session
undo-then-redo — acceptable loss; do NOT further complicate.

### Task 4 — Data hygiene purge (PENDING OWNER APPROVAL — OQ-7)
**File:** `suggestion.rs` (+ mod placement handled in Task 1).
**Spec:** pure fn `purge_invalid_pending_suggestions(db) -> usize`: demotes
`status == Pending` rows failing the ported judge `is_valid_pair` to
`status = Dismissed` — the SAME single decision core. Pre-existing
Dismissed/Accepted rows are NEVER modified; the dictionary store is NEVER
touched. Wired in the SAME locked read-modify-write and run
BEFORE `expire_stale_suggestions` in that block (reason: a >30-day garbage
Pending row must not expire into Dismissed before the purge pass gets it —
avoids permanent evasion). Invocation sites: the existing startup call site
(the startup branch is gated by `auto_learn_enabled` — main.rs:135 — so
first-start purge is conditional; the page-load path covers the disabled
case) and the page-load path (zero frontend change goal confirmed at
implement time; page-load expire already exists per
settings.js:94-95/281-282 wiring).
NO version-bump (nothing reads `db.version`; dropping the bump removes
meaningless churn — reviewer A6). Log purged count + per-reason breakdown at
info level.
  - [ ] Reversibility decision (OQ-7): RESOLVED — **DEMOTE-to-Dismissed**
        (approved owner default). Invalid Pending rows move to
        `status = Dismissed` (frequency/history preserved for audit),
        never Deleted. Same visible UX as a delete today because Dismissed
        rows never resurface (suggestion.rs:196-201 re-observation keeps
        Dismissed).
  - [ ] Expected count UNDER THE CORRECTED TABLE: 10 of the 11 seeded
        garbage rows (NOT 11 — `again→agent` sim 0.400 survives; failure
        mode of v2's claim fixed).
  - [ ] `shwande→Sinead`/short-corrected rows, if present as Pending, are
        removed/suppressed ONLY with OQ-1/OQ-2 owner approval (retroactive
        nature called out).
  - [ ] Purge tests validate: garbage Pending demoted, valid
        `shunade→Sinead` kept, pre-existing Dismissed + Accepted row COUNTS
        unchanged (status-count snapshot before/after, not total-Dismissed),
        dictionary untouched, idempotent second pass = 0, no
        store→dictionary calls (no test-through-real-dictionary-path).
**Owner data review (out-of-code):** if auto-accept was ever enabled,
pre-existing Accepted garbage may sit in the dictionary (Words tab) — check
`get_auto_accepted_suggestions`/"auto" badge rows manually; purge never
touches them. Auto-accept stays OFF (default) throughout.

### Task 5 (OPTIONAL, OQ-4) — upsert intra-batch dedup
`suggestion.rs::upsert_suggestions`: dedup incoming batch by candidate
canonical key before the loop (~5 lines + test). Frequency-accuracy fix only
(affects P-path batches too — acceptable, purely de-flating).

### Task 6 (NEW v3; v4 structure per reviewer round-2) — snippet echo shield
**Files:** `snippets.rs` (new small additive helper) + `suggestion.rs`
(prelude skip). `learner.rs` is NOT touched (v3's fingerprint dropped;
`get_current_dictionary()` remains the sole consumer of dictionary keys).
Hook (reviewer catch, verified by me in suggestion.rs:186-201):
`should_skip_new_candidate` (L168) runs ONLY in the new-row `else` branch
(L202) — an EXISTING row (e.g. `again→agent`) is found first (L192-196) and
still inflates at L198 on every take. Therefore the snippet-key check must
run in the upsert prelude BEFORE the existing-row branch, guarding BOTH
branches.
**Spec:** helper in `snippets.rs` returning active, non-deleted
`trigger → expansion` pairs (field `snippet.expansion`, snippets.rs:275 —
v3's "payload" wording corrected; master toggle respected; blank
trigger/expansion excluded). Upsert prelude builds `snippet_keys` via
`dictionary::canonical_entry_key(trigger, expansion)`; then
`if snippet_keys.contains(&candidate_key) { continue; }` — skips BOTH new
row creation AND existing-row frequency increment. Fail open: store read
error ⇒ empty set + warn log.
Motivation (reviewer hazard + data fit): a single-word snippet `again→agent`
would add +1 per dictation take — a plausible contributor to the observed
8x, and a structural echo pollution vector otherwise untouched.
  - [ ] test: snippet pair in store ⇒ identical-pair upsert creates NO new
        row AND does NOT increment an EXISTING row's frequency (both
        branches; the existing-row case is the v4 addition).
  - [ ] test: no snippet store / read error ⇒ fail-open (upsert normal).
  - [ ] multi-line expansions harmless (pair-key match is exact-string).

### Checkpoint
- [ ] `cargo test auto_learn && cargo test suggestion` green (incl. port
      conformance suite)
- [ ] `cargo check` for a non-windows target if cheaply available (module
      placement proof); else `npm run check` + gating-structure review
- [ ] `npm run check`, `npm run clippy`, `cargo fmt`
- [ ] Owner soak: dictate → correct one word ⇒ 1 row @1x; continue typing ⇒
      no new garbage rows; first-start purge is conditional on
      `auto_learn_enabled` (page-load path covers the disabled case).

## 5. Corrected pair-gate table (v4; similarity = 1 − dist/maxLen; floor 0.35)

| Pair | dist/max | sim | Verdict |
|---|---|---|---|
| stuck→agent | 5/5 | 0.00 | reject |
| CS50→tabs | 4/4 | 0.00 | reject |
| have→Type | 3/4 | 0.25 | reject |
| release→agent | 6/7 | 0.14 | reject |
| again→agent | 3/5 | **0.40** | **SURVIVES** (residual; dismiss ⇒ silent forever) |
| in→add | 3/3 | 0.00 | reject |
| Let→et | 1/3 | 0.67 | reject — corrected-length rule ONLY (dist 1: subsequence `et` ⊂ `Let`; ratio 0.333 PASSES — no double-kill; length gate is load-bearing) |
| do→add | 2/3 | **0.333** | reject by 0.017 margin (tightest in set) |
| check→add | 5/5 | 0.00 | reject |
| testing→ChatGPT | 6/7 | 0.143 | reject — dist pinned in test (`assert_eq!(edit_distance("testing","chatgpt"), 6)`, Round-4 F6) |
| in→to | 2/2 | 0.00 | reject (length + ratio) |
| leak case release→replace | 3/7 | 0.571 | **caught by LCS guard, not gates** |

10/11 screenshot pairs die at the gates; 1 survives at 1x-per-context and is
permanently Dismissable; the rewrite leak (release→replace) dies at the
guard. RC-1 boundary note (v4): the port's single adjacency pair
`cool→Good` also survives (dist 2/4, sim 0.50) — boundary-word residual
class, owner-accepted, same tier as `again→agent`. Kept-passing:
shunade→Sinead (0.43), Johnatan→Jonathan, definately→definitely (0.9),
recognision→recognition, accomodate→accommodate, abc→axc. Case-only pairs:
SKIPPED per OQ-5 (lowercase-identical gate).

## 6. Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| OQ-1 `shwande→Sinead` (0.286) stops surfacing — RESOLVED (owner accepted; manual Words-tab add) | **Blocker decision — accepted** | No floor admits it without re-admitting `do→add` (0.333). Manual add is the fallback. Soundex carve-out considered and rejected (complexity; out of scope) |
| OQ-2 short corrected words (<3) stop surfacing — RESOLVED (accepted) | Low | Reference-parity; manual add possible |
| OQ-5 case-only stop surfacing — RESOLVED (owner chose SKIP, reference parity) | Low | Words fixable manually |
| Newly interpreted rewrites stop learning (contiguous multi-word replacements, `their quiet form`→`there quite from`) | Med | Known, reference-parity consequence; old code DID learn these (3 pairs @0.4-0.5 sim); flagged residual — later enhancement possibility (positional pairing of equal-length runs) recorded, NOT in this batch |
| Frequency grows more slowly (session memory) | Low | Cross-dictation counting still accrues; per-reviewer "not severe, but real" |
| Purge misjudgement | Low | Same judge code as live extractor; OQ-7 RESOLVED = DEMOTE-to-Dismissed (approved, reversible); pre-existing Dismissed/Accepted/dictionary immune |
| Port drift | Low | Line-anchor header + conformance tests |
| Compile coupling (reviewer A5) | Medium | Module outside windows gate (verified; placement pinned in Task 1 acceptance) |
| `again→agent`-class residual reaches auto-accept if owner enables auto-accept | Med | Owner instruction: keep `auto_accept_enabled` OFF until tab is clean; auto-accept adds 3-observation cross-day bar beyond human dismiss |

## 7. Open questions (owner) — RESOLVED 2026-10-20 (owner: "accept all defaults as recommended")

- **OQ-1 (blocker): ACCEPTED** — `shwande→Sinead` no longer auto-surfaces;
  manual Words-tab add is the fallback. No floor change attempted.
- **OQ-2: ACCEPTED** — corrected words <3 chars stop surfacing (reference
  parity).
- **OQ-3 (choice): 0.35** — exact reference parity (recommended default).
- **OQ-4 (choice): Task 5 INCLUDED** (batch dedup ships).
- **OQ-5: SKIP** — case-only pairs skipped, matching the reference exactly.
- **OQ-6: EXACT PARITY** — numeric pairs judged by the port gates only
  (`123→456` flips to rejected; `test_accept_number_to_number` flips).
- **OQ-7: DEMOTE-to-Dismissed** — purge demotes invalid Pending rows to
  Dismissed (reversible, invisible today since Dismissed rows never
  resurface; identata zero data destruction).

## 8. Review round-1 adjudication (transparency)

Accepted: A1 (LCS guard), A2 (10/11 corrections + RC-1 third pair),
A3 (numeric — resolved via OQ-6), A4 (flip ledger + corrected arithmetic:
have→Type 0.25, do→add 0.333, again→agent 0.400, Let→et length-kill),
A5 (neutral module; independently verified mod.rs gating), A6 (purge
ordering, no version bump,Demote option, reason logging), A7 (OQ-1 as
blocker), A8 (session-memory scoping), A9 (multi-word replacement residual
documented + test), A10/A11 (reword P-path claims; recompute examples
against real code — table v3 §5).
Nuanced (not accepted as stated, reasoning kept): reviewer G's "do not add the reference's corrected-word dedup/skip" —
ACCEPTED for skip (pair-level
keeps; v2's union idea retired); dedup-by-pair remains (store semantics).
 Advocate holds: exact parity is the behavior target; safety extras (guard,
placement) exist because Fluence's learned output is rewrite-active, which
the reference's word list is not.
Rebutted: none rejected outright — no verifiable errors found in the
reviewer's checks; the reviewer's `release→replace` distance cited as sim
0.571 reconciles with my recompute (dist 3/7).

### Round 2 (final reconciliation)

Reviewer verdict: AMEND — narrow, acceptance-criteria only, no design
changes. All deltas adjudicated; 10 accepted, 1 rebutted.

Accepted (each verified in code where it mattered):
- (a) OQ-6 exact-parity confirmed; digit-mix veto NOT ported — the two
  existing mixed-number tests stay green via the gates (sim 0.0).
- (b) Task-6 structural catch VERIFIED by me: `should_skip_new_candidate`
  runs only in the new-row `else` branch (suggestion.rs:202); the check now
  precedes the existing-row branch (L190-196), guarding both branches;
  `snippet.expansion` naming verified (snippets.rs:275); `learner.rs`
  fingerprints dropped; fail-open + existing-row test added.
- (c) Purge DEMOTE wording + status-count-snapshot test (not total-Dismissed).
- RC-1 expected pair set was old-code output — corrected to port output.
- Case-only ledger/§5 contradiction resolved (OQ-5 decided SKIP; the two
  flip tests verified to exist at ui_automation.rs:433/440).
- Let→et arithmetic corrected: dist 1/3 (subsequence), sim 0.667, ratio
  passes — the corrected-length gate is the SOLE killer (marked
  load-bearing). No double-kill.
- a→the sim corrected 0.33 → 0.00; "five JS skips" → four gate-skips + D4;
  definately→definitely 0.8 → 0.9; startup purge inherits
  `auto_learn_enabled` (main.rs:135), page-load path covers the disabled
  case.

Rebutted (my verification, not opinion): the reviewer's "the final 'zero
candidates' is still correct" claim for the RC-1 shape is false. The port's
single adjacency pair `cool→Good` PASSES the gates: dist(cool, good) = 2
(c→g, l→d), ratio 2/4 = 0.5 ≤ 0.65, sim 0.50 ≥ 0.35, corrected len 4 ≥ 3.
Expected surviving candidates for that shape = exactly 1 (boundary-word
residual class, owner-accepted). Plan asserts ONE, not zero.

Post-fix: plan is READY-TO-IMPLEMENT — owner OQs all resolved (§7).
Pending: reviewer round-3 final ACK on this reconciliation (esp. the
rebutted RC-1 point).

### Round 4 (audit corrections — REQUEST-CORRECTION → implemented)

Reviewer audit verdict: REQUEST-CORRECTION. The implement fork ("F1-F3 +
rerun tests") was answered and approved, but the reviewer ended its session
without making edits — all fixes were implemented by the owner-operator,
per the audit text. Disposition:

- F1 (MAJOR, accepted+fixed): the old dedup test was vacuous — its fixture
  produced ZERO candidate pairs and every assert compared a length to
  itself. Replaced with two independent regions separated by shared words:
  extract("the shunade is fine shunade", "the Sinead is fine Sinead") →
  exactly 1 pair ("shunade", "Sinead"). Prod dedup (seen_pairs) untouched.
  (The auditor's own suggested fixture was predicted to yield zero pairs
  via traceback; the empirical test run settled it.)
- F2 (MINOR, accepted+fixed): new log-only mirror
  `invalid_pair_reason()` in decision_core.rs walking the SAME gates
  in the SAME order (`is_valid_pair` now delegates to it; decisions still
  flow through the boolean). Purge demote logs now carry the per-row
  reason (per plan Task 4's per-reason breakdown); the upsert `Skip` arm
  logs two distinct reasons — snippet echo shield vs already-in-dictionary
  (sound: the snippet check mirrors classify_candidate's first gate).
  Gate-ORDER test added; note the empirical correction: do→add's first
  failing gate is SIMILARITY (corrected "add" is 3 chars, dist 2/3 =
  0.667 > 0.65), not the length rule — the ("shunade", "dr", len-2)
  pair covers `<3` and the known-gate order proof uses do→add known-wins.
- F3 (MINOR, accepted+fixed): `cached_store_checked()` returns the load
  result with identical cache semantics (including caching the degraded
  default on error, so a read failure surfaces once per cache generation
  instead of every take); the echo-shield wrapper turns that signal into a
  `warn` via the pure seam `shield_keys_for(Err(ok))` — Err → empty key
  set + one logged warn; Ok → normal filter. Two hermetic seam tests
  (no shared-root store touched — the F7 zone stays out of new tests).
  Cached wrapper simplified to `unwrap_or_default()` for clippy.
- F4 (accepted+fixed): ui_automation's quote test comment now claims
  judge-level ONLY (dist 2/7 = 0.286 passes the gates); the pipeline
  truth — extractor strips edge quotes on both sides, tokens equal, NO
  candidate exists — is asserted by
  decision_core `test_extract_quoted_tokens_form_no_pair` (both
  directions).
- F5 (accepted+fixed): tokenize doc no longer claims exact
  `is_alphanumeric ≡ \p{L}+\p{N}` — it states the Rust trait set is the
  Alphabetic ∪ Number SUPERSET (includes Other_Alphabetic), behaviorally
  identical for BMP Latin/Cyrillic word text.
- F6 (accepted+fixed): plan §5's "exact value asserted in test" is now
  literally true: `assert_eq!(edit_distance("testing", "chatgpt"), 6)` —
  pinned EMPIRICALLY (6/7 ≈ 0.857); the v4 guess of 5 was the
  owner-operator's, the test run corrected it.
- F7 (accepted, REPORT-ONLY per audit): pre-existing flaky
  `snippet_save_merged_refuses_corrupt_file_then_recovers_fresh` (shared
  cfg(test) store root). Round-4 evidence confirms entirely preceding
  state: fails batched at ≥4 threads, passes isolated in BOTH binaries
  and in the suite with it excluded (22/22 lib + bin). NO Round-4 change
  touches that test or its zone. Owner follow-up: separate test-isolation
  fix (S/M), outside plan-2.

Additional hygiene while validating (clippy `--all-targets` surfaced
cfg(test) warnings the earlier plain-clippy baseline could not see):
fixed `snippets.rs:219` manual_unwrap_or_default (new in Round-4),
removed empty line after doc comment before ui_automation tests (line
~356), and converted four Round-3 purge tests to scope-initializer form
(`field_reassign_with_default`). Non-test behavior unchanged.

### Round 4, second pass (revalidation fixes)

Reviewer revalidation verdict: REQUEST-CORRECTION, one substantive item:
- F3 revalidation (accepted+fixed): the Round-4 warn lived in the shield
  seam (`shield_keys_for`), but the normal call order — `cached_store()`
  (expansion take) runs FIRST from `process_transcript`, hits the
  load-error branch, caches the degraded default and silently discards the
  Err; the later shield wrapper then sees a cache-HIT Ok(default) and never
  warns. Fix: the warn now lives in `cached_store_checked`'s load-error
  branch — the single point where the degraded default is cached — so the
  record fires exactly once per cache generation for BOTH read-only
  consumers, regardless of call order; `shield_keys_for` is now a pure
  degrader (Err → default, no log). New hermetic ordering test
  `test_shield_keys_empty_with_cache_populated_degraded` seeds
  STORE_CACHE exactly as a degraded load leaves it (guard-serialized,
  direct cache wiring — the F7 fs zone stays untouched) and asserts the
  shield yields an empty key set through the cache-first path.
- Cosmetic (accepted+fixed): stale `do→add` comment in
  suggestion.rs purge test (`// len 2 < 3` → `// dist 2/3 = 0.667 > 0.65`,
  matching §8's first-pass empirical record); plan §5 testing→ChatGPT row
  now records the exact 6/7 (0.143) pinned by test.

Reviewer confirmed-fixed in the first pass: F1, F2 (code), F4, F5, F6 +
F7 record-only disposition adequate + no other regressions
(auto_learn 117/117, suggestion 34/34, snippets 22/22 skip, flake 1/1,
fmt/check clean, only pre-existing snippets.rs:90 clippy warning).

Post-second-pass checkpoint (2026-10-20): snippets 24/24 BOTH binaries
(includes the new ordering test; the F7 flake passed this run too),
suggestion 34/34 (lib+bin); fmt clean; clippy --all-targets again shows
our four files clean except the pre-existing snippets.rs:90 derivable_impls.

**Reviewer ACK (Round 4, second pass — 2026-10-20):** all revalidation
items resolved, each confirmed with line evidence (warn in
`cached_store_checked` load-error branch; pure `shield_keys_for`; ordering
test coverage; both doc nits). Reviewer re-verified: suggestion 34/34,
snippets 24/24 serial; reproduced the F7 flake in the full-parallel run
(known, record-only, separate follow-up); fmt clean. Plan-2 review cycle
CLOSED — remaining item: owner soak (§9).

## 9. Verification commands

Toolchain note (recorded 2026-10-20, Round 4): cargo is NOT on the system
PATH, and .msvc lives under `Fluence-Windows\.msvc` with no vcvars64.bat at
either guessed path. `npm run check` / `npm run clippy` cannot find cargo in
their subshells. Use the project's own bootstrap from `src-tauri`:

```
. ..\rust-env.ps1 ; cargo fmt
cargo test auto_learn     # expect: 117 passed (lib) + 117 passed (bin)
cargo test suggestion     # expect: 34 passed (lib) + 34 passed (bin)
cargo test snippets -- --skip snippet_save_merged_refuses_corrupt_file_then_recovers_fresh
                          # expect: 22 + 22 passed; the skipped test is the
                          # pre-existing F7 flake, green in isolation:
cargo test snippet_save_merged_refuses_corrupt_file_then_recovers_fresh
cargo check               # no errors
cargo clippy --all-targets
```

Round-4 baseline (2026-10-20): all four touched files carry zero clippy
warnings except pre-existing `snippets.rs:90` (derivable_impls, audit-acknowledged).
Manual soak by owner (`npm run dev`, RUST_LOG=info), then purge verification
on Suggestions tab.
