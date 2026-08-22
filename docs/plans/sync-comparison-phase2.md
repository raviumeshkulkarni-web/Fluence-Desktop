# Phase 2 — Architectural Comparison: Independent Design vs Existing Sync Implementation

**Status:** Analysis only (no code modified)
**Compared:** A = Phase-1 independent architecture (`cross-platform-sync-architecture.md`) · B = actual `sync-feature` implementation on both repositories ("frozen v1.1")
**Method:** Full read of the runtime code paths on both branches; every claim below cites a file.

---

## PART 1 — EXECUTIVE VERDICT

- **The existing "frozen v1.1" core is architecturally sound and convergent with the independent design**: shared domain files on Drive appDataFolder, per-record LWW with tombstones, union-dedup stats, PKCE OAuth with `drive.appdata` only, deterministic merges. Both designs independently arrived at the same shape — that is mutual validation.
- **But the existing implementation does not yet deliver the headline product promise.** Account-level combined statistics are plumbed end-to-end but **never displayed** (no UI reads `stat_sync` / `stats_sync.json`; HomeScreen sums local-only `stats_daily`, Windows stats query local history), and **Windows never creates fresh stat events** (`create_fresh_event` has zero callers) — Windows stops contributing after first enrollment while Android contributes per dictation.
- **Deletion is unintentionally permanent.** Winner ordering is `(tombstoneBit, updatedAt, deviceId)` — a tombstone beats any live record regardless of recency, tombstones are never GC'd, identity is businessKey, and editing deleted entries is rejected. Once a deletion is pushed, that word/trigger can never live again on the account (re-adds silently lose to the old tombstone).
- **The concurrency defense is probably inert.** Both platforms PATCH with `If-Match` and carry 412-retry machinery, but Drive API v3 dropped If-Match support. Convergence still holds (item-LWW + local retention self-heal next pass), but the design's central write-conflict guard is likely dead code. Must be verified at runtime.
- **~5,600 lines of dead legacy code** remain in the Windows runtime tree (per-record engine `engine.rs` 3,066 LOC, `wire.rs`, `quarantine.rs`, `settings_store.rs`, `legacy.rs` stub, examples) from an earlier history-syncing generation. The v1.1 scheduler never calls them, yet quarantine commands are still registered.
- **Asymmetric maturity between platforms**: Android's settings sync (value-diff per key) is correct; Windows' is effectively receive-only with dead outgoing paths and a hardcoded `dictionary_enabled="true"`. AccountHash is 64-hex on Windows, 16-hex on Android.
- **Verdict: EVOLVE, don't restart.** The frozen v1.1 core (auth, drive stores, merge, schedulers, stores, tests) is production-quality and matches what an independent first-principles process produced. Restarting would re-pay ~2 platforms of working infrastructure for zero correctness gain. But ship-blocking semantic fixes are required before this is the product behavior the owner specified.
- **Keep:** domain-file model, businessKey identity (better than pure-UUID for duplicate creation), monotonic maxSeen clock floor, corruption-skip, deterministic serialization, scheduler polish (single-flight, backoff, WorkManager, panic guard), Android settings value-diff store.
- **Remove:** the entire legacy per-record layer + quarantine UI surface (~5.6k LOC), dead settings/stats paths on Windows.
- **Fix:** stats freshness (Windows) + combined display (both), re-creation semantics (pure LWW), verify/replace If-Match assumptions, unify accountHash, ingest validation (recompute businessKey from content, size caps).
- **Complexity truth:** existing ≈9.5k LOC Rust sync (≈3.9k active) + ≈3.6k LOC Kotlin vs ≈1–1.3k/platform estimated independently. The overage is mostly the dead legacy layer and copy-pasted per-domain functions — not inherent complexity.

---

## PART 2 — DECISION TABLE

| # | Area | Independent design (A) | Existing implementation (B) | Winner | Reason |
|---|------|------------------------|------------------------------|--------|--------|
| 1 | Account identity | Google account via OAuth | Same (PKCE, email→SHA256 accountHash) | **Equivalent** | Identical model |
| 2 | OAuth / Drive | appDataFolder, PKCE, tokens in OS secure stores | Same; scope `drive.appdata` only; access token memory-only; Win loopback port + optional client secret | **B slightly** (memory-only access token discipline, tested PKCE vectors); B's optional client-secret provisioning is deployment friction | — |
| 3 | Account vs device-local boundary | dict+snips+stats sync; history/secrets/platform-settings excluded | Same exclusions in v1.1; 5-key settings whitelist synced (A deferred all settings) | **B** | Settings whitelist is a real requirement A under-scoped; Android implements it well |
| 4 | Win↔Android semantics | Canonical collections, platform mapping documented | Same mapping (kind=expansion ⇔ Snippets; isEnabled preserved) | **Equivalent** | — |
| 5 | Replacement-device restore | Merge-down + push-up on first login | `stampUnstamped` + GET/merge/PUT achieves same | **Equivalent** | Both restore account state; see Part 3 for stats gap |
| 6 | Dictionary sync | UUID-id records, LWW(mtime,origin), tombstones | businessKey identity, LWW(tombstone,updatedAt,deviceId), eternal tombstones | **A** | B's delete-dominance makes deletion permanent (defect); B's businessKey handles duplicate creation better than A's id-union |
| 7 | Snippet sync | Same as dictionary | Same machinery | **Equivalent** (same delete flaw) | — |
| 8 | Statistics sync | Per-device ledgers summed at display; always fresh | Event-sourced union by eventId; Android fresh ✓, **Windows never creates events ✗** | **A** | A's model cannot forget to contribute; B is half-wired |
| 9 | Activity aggregation | Daily buckets summed across shards, displayed | Events synced but **never displayed anywhere** | **A** | Product promise unmet in B |
| 10 | Time-saved / monthly metrics | Derived from merged buckets at display | Local-history-derived only; month helpers exist but not account-level | **A** | — |
| 11 | History isolation | Never leaves device | Explicitly enforced in v1.1 ("history stays local"); legacy engine retained but unused | **Equivalent** (B carries dead history-sync baggage incl. `transcription_history.syncId` migration) | — |
| 12 | Device identity | Random UUID per install | Same (UUIDv4, persisted metadata) | **Equivalent** | — |
| 13 | Remote storage model | Per-device shard files (write-isolated) | 4 shared domain files + If-Match optimistic locking | **A** | A's single-writer-per-file makes conflicts structurally impossible; B depends on If-Match, which Drive v3 likely ignores |
| 14 | Data model | Envelope {v, records[], stats} | Four envelopes {v, entries[]}; embedded sync columns in user stores | **A slightly** | B pollutes user stores with 7–11 sync fields and needed Room migrations to v7; A keeps sync memory separate |
| 15 | Conflict resolution | Pure LWW (mtime, deviceId) | LWW with delete-dominant bit; businessKey grouping | **Split** | B's businessKey grouping better; B's delete dominance worse (permanent deletion) |
| 16 | Concurrent edits | Deterministic by total order | Deterministic by total order (+412 retry that likely never fires) | **Equivalent** | — |
| 17 | Delete propagation | Tombstone competes by mtime | Tombstone dominates always | **B safer against resurrection, A correct overall** | B's safety costs re-creation; A's ordering already prevents resurrection of *older* copies |
| 18 | Tombstones | Forever (tiny data) | Forever, plus hard-delete of never-pushed ones locally | **Equivalent** | B's everPushed refinement is nice |
| 19 | Offline operation | Full-state, duration-independent | Full-state, duration-independent | **Equivalent** | — |
| 20 | Retry behavior | Next-trigger retry | Backoff 1s→60s, fatal latch, WorkManager retry, single-flight | **B** | Production-grade scheduling polish A didn't specify |
| 21 | Idempotency | By construction (state-based) | By construction (state-based) + etag bookkeeping | **Equivalent** | — |
| 22 | Crash recovery | Next sync repairs | Same + panic-guarded scheduler latch | **B slightly** | — |
| 23 | Partial network failures | Whole-file semantics | Whole-file semantics + timeout classification | **Equivalent** | — |
| 24 | Auth expiry | Refresh once → re-login state | Same, classified `AuthRequired`, UI surfaced | **Equivalent** | — |
| 25 | Clock skew | Accepted, documented | Persisted maxSeen monotonic floor | **B** | maxSeen genuinely improves backwards-clock behavior |
| 26 | Schema evolution | v-field + ignore-unknown | v-field strict (=1) + parse-fail skip-domain | **A slightly** | B's strict `v!=1 ⇒ corrupt` skips whole domains instead of forward-reading |
| 27 | Security/privacy | Least privilege, no E2EE (documented) | Least privilege, memory-only tokens, hashed email | **B slightly** | Token hygiene details are stronger |
| 28 | Corruption handling | Quarantine bad peer shard, roundtrip-validate own | Skip-corrupt per domain, duplicate-file handling, deterministic bytes | **B** | More battle-tested (duplicate files, md5 fallbacks) |
| 29 | Sync triggers | Debounce/throttle/cadence/manual/login | Same set + foreground gating + WorkManager | **B** | Mobile lifecycle handling is real-world necessary |
| 30 | First-login behavior | Merge-down + push-up simultaneously | stampUnstamped → dirty → pushed up; remote newer wins | **Equivalent** | See poison-path risk in Part 6 |
| 31 | Replacement-device behavior | Restore account state, fresh contribution | Works for dict/snips; stats restore invisible & Windows stops contributing | **A** | — |
| 32 | Account switching | Cache namespaced by account; document visibility | Rows hash-stamped; loads filtered by hash; UI doesn't partition visibility | **Equivalent-ish** | Neither leaks uploads; both keep foreign content visible locally |
| 33 | Orphaned/lost devices | Frozen ledgers harmless by construction | Stale rows lose LWW; stats events persist forever (correct) | **Equivalent** | — |
| 34–37 | Code complexity / files / tests / maintainability | ~1–1.3k LOC/platform, 1 new file/device, no user-store schema changes | ≈9.5k LOC Rust (≈5.6k dead) + ≈3.6k Kotlin; Room v4→v7; 11 sync fields in user stores; strong test suites | **A on size, B on test coverage** | Dead legacy layer is the main maintainability tax |

---

## PART 3 — PRODUCT SEMANTICS AUDIT (existing implementation)

| Requirement | Verdict | Evidence |
|---|---|---|
| Account-level dictionary/snippets | ✅ Correct | `frozen.rs` / `V1SyncEngine.kt` merge+push both ways; businessKey dedup |
| Account-level statistics — contribution | ⚠️ Half | Android creates an event per dictation (`HistoryRepository.kt:44-52`); **Windows' `create_fresh_event` is dead code** — post-enrollment Windows dictations never reach the account |
| Account-level statistics — visibility | ❌ Missing | No reader of `stat_sync`/`stats_sync.json` exists in any UI; HomeScreen sums local `stats_daily`; Windows `get_history_stats` queries local history. X+Y is synced but never shown |
| Stats survive history deletion | ✅ (design) / ⚠️ (effect) | Android explicitly never touches stat_sync on clear; Windows hard-deletes history but stats_sync.json persists — yet since Windows adds no new events, its contribution freezes at enrollment snapshot |
| Replacement-device restore (dict/snips) | ✅ | GET→merge→apply on sign-in restores into Room/prefs/JSON |
| Replacement-device restore (stats/activity) | ❌ Effectively no | Restored events are never rendered; user sees zeros/local-only |
| History isolation | ✅ | v1.1 never uploads history; legacy path dormant |
| Secrets/model config exclusion | ✅ | Whitelisted keys only; provider/hotkey fields absent from wire format |
| Platform-specific settings exclusion | ✅ | Only 5 user-preference keys whitelisted |
| First-login doesn't clobber newer remote | ✅ | LWW protects newer remote winners |
| Deleted-entry resurrection | ✅ Prevented (over-tightly) | Delete-dominant order prevents resurrection **and** legitimate re-creation |
| Account-switch leak | ⚠️ Partial | No cross-account upload (hash-stamped loads); foreign content remains visible locally after switch |

---

## PART 4 — FAILURE-MODE COMPARISON

| Failure mode | Independent (A) | Existing (B) |
|---|---|---|
| Two devices add same word offline | Two live duplicates until platform dedup on import | One winner immediately (businessKey) — **B better** |
| Two devices edit same entry offline | Newer mtime wins deterministically | Same (newer updatedAt wins) — equivalent |
| Delete vs concurrent edit | Newer event wins (edit can survive if genuinely later) | Delete ALWAYS wins — deterministic, but swallows later edits and blocks re-adds forever |
| Simultaneous sync (both PUT) | Impossible to conflict (own shard only) | If-Match 412 dance — **likely inert on Drive v3**; falls back to file-level last-write-wins; converges next pass via item-LWW + local retention |
| Upload ok, response lost | Harmless (state-based) | Harmless (etag refresh next pass) |
| Crash mid-pass | Next sync repairs | Same + panic latch keeps scheduler alive |
| Device offline months | Identical path | Identical path |
| Backwards clock | Skewed writes may win ties | maxSeen floor prevents device's own timestamps regressing — **B better** |
| Corrupt peer envelope | Quarantine shard, continue | Skip domain this pass, continue — equivalent; B also handles duplicate files |
| Corrupt own upload | Roundtrip-validate pre-PUT | Not present — **A has a guard B lacks** |
| Auth revoked mid-pass | Re-login required state | Same, classified + surfaced |
| Zero-timestamp legacy row uploaded | N/A (fresh ids) | **Risk:** Windows `validate()` rejects `updated_at<=0` items ⇒ whole envelope treated corrupt ⇒ Windows ignores entire domain. Needs a regression test on Android legacy-row enrollment |
| Stats double counting | Impossible (device-ledger sum) | Impossible (eventId union) — equivalent once Windows emits events |

---

## PART 5 — COMPLEXITY COMPARISON

**Existing (measured):**
- Windows branch diff vs main: **+11,927 lines** total. Sync module ≈**9,500 LOC** (committed: auth 563, drive 807, engine 3,066, scheduler 1,213, wire 724, settings_store 543, quarantine 491, mod 15; untracked frozen/domain/merge/clock/metadata/stores/legacy ≈2,084). Runtime-active ≈**3,900**; dead legacy ≈**5,600**. Data-layer diffs: dictionary +540, history +1,120, snippets +416, frontend +290.
- Android: sync package **28 files ≈3,600 LOC**, Room migrations v4→v7 (stat_sync + sync_metadata tables, sync columns on custom_dictionary AND transcription_history), extended snippet JSON schema.
- Four near-identical domain sync implementations per platform (copy-paste generalization opportunity).

**Independent estimate:** ~1,000–1,300 LOC/platform, one new file per device, no user-store schema changes.

**Justified extra complexity in B:** scheduler robustness (backoff/latch/single-flight/WorkManager/foreground), corruption/duplicate handling, per-domain mutexes, test suites. These earn their keep.

**Unjustified:** the entire legacy per-record generation (engine/wire/quarantine/settings_store/legacy/examples + registered quarantine commands + `transcription_history.syncId` migration); Windows' dead settings/stats outgoing paths; 11 embedded sync fields where a side-table/shard would isolate concerns; quadruplicated domain loops.

---

## PART 6 — SECURITY COMPARISON (concrete)

**Strengths in B (keep):**
- Scope limited to `drive.appdata`; PKCE S256 + state CSRF; Android public client with no secret; refresh token in Credential Manager / encrypted prefs; access token memory-only; account email only stored hashed; transcript text absent from every payload; no API keys in any wire type; fail-closed parsing (unparseable ⇒ skip, never apply).

**Weaknesses / required fixes in B:**
1. **If-Match reliance unverified** — if Drive v3 ignores it (expected), the documented concurrency guarantee is false. Fix: verify empirically; document file-level last-write-wins + item-LWW convergence as the actual mechanism (safe), or drop the pretense.
2. **No ingest size caps** — a hostile/broken peer envelope can be arbitrarily large (Drive quota is the only bound). Add max-bytes/max-items per domain.
3. **Wire-trusted `businessKey`** (Android parser takes it from JSON; Windows derives from content). Recompute from content on parse on both platforms.
4. **Validation asymmetry poison path** — Windows rejects `updatedAt<=0` items ⇒ whole domain skipped. Either validate per-item (skip item, not domain) or guarantee timestamps at enrollment (test it).
5. **Windows client secret via env/file** — acceptable for dev, but shipping requires either a secret-less installed-client flow (like Android) or a bundled secret rotation story. Align with Android's public-client flow.
6. **AccountHash length mismatch** (64 vs 16 hex) — unify to full 64-hex before any cross-referencing code ever appears.
7. Quarantine IPC commands registered but backed by dead engine — attack/maintenance surface with no function; remove.

---

## PART 7 — REQUIRED CHANGES

**A. KEEP EXACTLY AS-IS**
- Domain-file layout `appDataFolder/fluence/v1/*.json`; GET→merge→PUT pass shape; businessKey identity; union-by-eventId stats; monotonic maxSeen; corruption-skip; deterministic serialization; OAuth scope/token storage; scheduler cores (both platforms); Android settings value-diff store; tombstone-forever policy; hard-delete of never-pushed tombstones.

**B. KEEP BUT SIMPLIFY**
- Collapse four copy-pasted domain loops into one generic function per platform.
- 412 machinery: keep header (harmless), simplify retry path once If-Match reality is verified.
- Embedded sync columns: stop adding more; consider a side-table long-term (not a rewrite).

**C. MODIFY**
- Winner order `(tombstoneBit, updatedAt, deviceId)` → pure `(updatedAt, deviceId)`; allow editing/re-creating over tombstones. Deletes still propagate whenever they are the newest event.
- Windows: wire fresh stat-event creation into the history commit path (mirror Android).
- Unify accountHash to 64-hex on Android.
- Windows settings store: adopt Android's value-diff meta approach; remove hardcoded `dictionary_enabled`.
- Parsers: recompute businessKey from content; per-item validation (skip bad item, not whole domain); enforce payload caps.

**D. REMOVE**
- Windows legacy layer: `engine.rs`, `wire.rs`, `quarantine.rs`, `settings_store.rs`, `legacy.rs`, `examples/sync/`, quarantine Tauri commands, `HistorySyncStore`. (~5.6k LOC)
- `transcription_history.syncId` usage (leave column dormant; do not propagate).

**E. MISSING — MUST ADD**
- Combined-stats display on BOTH platforms: totals = Σ merged account events (+ local dirty), feeding HomeScreen/stats UI when signed in; local-only fallback otherwise.
- Monthly time-saved / activity derived from the merged event set at display time.
- Runtime verification test for If-Match behavior; document actual concurrency semantics.
- Regression test: legacy-row enrollment produces valid envelopes on the other platform.
- Own-upload roundtrip validation (serialize→parse before PUT).

---

## PART 8 — FINAL ARCHITECTURE ("frozen v1.2")

Base: **the existing frozen v1.1 domain architecture**, evolved in place on the sync branches. Not the Phase-1 shard design (its write-isolation advantage is real but incremental; v1.1's shared-domain model with verified-or-documented convergence is adequate and already built/tested on two platforms), and not the status quo (it fails the product's headline statistics promise and permanently deletes data).

1. **Remote model unchanged:** 4 domain files, envelope `{v, entries[]}`.
2. **Merge law changed in one place:** winner = `max(updatedAt, deviceId)` per businessKey. Tombstones remain records with `deletedAt`; they win exactly when they are newest. Re-creation becomes possible; resurrection of older copies remains impossible.
3. **Stats completed:** both platforms emit one event per dictation into the union; display layer computes lifetime/weekly/monthly/account totals from the merged set (+local dirty) on both platforms; deterministic day-rollup compaction behind a size ceiling (>256 KB) as the growth escape hatch.
4. **Hardening:** per-item validation, content-derived businessKeys, ingest caps, pre-PUT roundtrip, unified accountHash, public-client OAuth parity on Windows.
5. **Deletion of the legacy generation** so the maintained surface ≈ active surface (~4k LOC/platform including UI).
6. **Documented residual risks:** file-level last-write-wins window between simultaneous PUTs (self-healing next pass); account-switch keeps foreign content visible locally (policy, not leak); clock skew affects which valid value wins only.

This hybrid keeps everything B proved out in production-facing detail, adopts A's stats freshness/display discipline and re-creation semantics, and deletes everything neither needs.
