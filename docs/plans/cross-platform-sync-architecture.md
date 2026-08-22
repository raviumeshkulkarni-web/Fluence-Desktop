# Cross-Platform Synchronization Architecture — Fluence (Windows + Android)

**Status:** Proposal (architecture phase — no code written)
**Scope:** Windows (Tauri v2 / Rust) + Android (Kotlin / Room), designed from `main` on both repositories.
**Philosophy:** Extremely simple, extremely robust. Nothing in this design exists without a named requirement that justifies it.

---

## 1. Current architecture relevant to synchronization

### 1.1 Product shape

Both platforms are local-first voice-typing apps. There is **no account system, no backend, no licensing server** anywhere on either `main` branch. All features run locally; users bring their own STT/LLM API keys (Groq/OpenAI/Mistral/custom).

### 1.2 Data inventory (Windows, `%LOCALAPPDATA%\Fluence\`)

| Store | File | Format | Contents |
|---|---|---|---|
| Settings | `settings.json` | JSON | Hotkeys, overlay prefs, audio device, provider configs (`base_url`, `model`, `api_key_saved` flag), language, theme, feature toggles |
| History | `history.db` | SQLite (`history` table) | `{id UUIDv4, timestamp RFC3339, text, mode, duration_ms, provider, char_count}`; append + delete-one + clear-all |
| Dictionary | `dictionary.json` | JSON array | `{id UUIDv4, spoken, corrected, kind: correction\|expansion}` |
| Snippets | `snippets.json` | JSON | `{enabled, snippets: [{id UUIDv4, trigger, expansion}]}` |
| Suggestions | `suggestions.json` | JSON | Auto-learn candidates: frequency counts, status lifecycle, expiry |
| Secrets | Windows Credential Manager | OS store | API keys under `Fluence/…` namespace |

Stats on Windows are **derived queries over the history table** (`get_history_stats`, `get_weekly_activity`). Clearing history therefore zeroes stats today.

### 1.3 Data inventory (Android)

| Store | Mechanism | Contents |
|---|---|---|
| Room DB `fluence_database` v4 | SQLite via Room | `transcription_history` (auto-increment Long id), `custom_dictionary` (auto-increment Long id, unique `spokenText`, `isEnabled`), `suggestion_history`, `stats_daily` |
| Snippets | SharedPreferences `fluence_prefs` → one versioned JSON doc | `{id Long, trigger, expansion}` + global enabled flag |
| All other settings | SharedPreferences | Device/IME config, privacy exclusions, update prefs |

Key observation: Android's `stats_daily` is an **independent aggregate ledger** (`day → {wordCount, dictationMs}`), incremented transactionally at dictation-commit time and *not* touched by history clear. This is exactly the right primitive for account-level statistics.

### 1.4 Structural facts that constrain the design

- **IDs diverge:** Windows uses UUID strings everywhere; Android uses auto-increment Longs for dictionary and snippets.
- **Dictionary semantics diverge:** Windows folds "expansion"-kind entries into the dictionary; Android models them as separate Snippets. Android has per-entry `isEnabled`; Windows does not.
- **History fields diverge** (mode/isAgentMode, char_count vs wordCount, RFC3339 vs epoch-millis) — irrelevant, since history does not sync.
- Existing reusable infrastructure: atomic tmp+rename file writes and corrupt-file quarantine (Windows settings/suggestions), Credential Manager access (Windows), OkHttp + `androidx.security.crypto` + WorkManager (Android), reqwest/rustls/uuid/chrono/sha2/base64 already in the Windows Cargo tree.

---

## 2. Synchronization requirements (derived from product decisions)

Stated by the product owner; treated as hard requirements:

1. **Google account sign-in** using Google Drive OAuth. The user's own Drive is the storage. No Fluence server.
2. **Account-level statistics:** words dictated, typing time saved, dictation time, monthly time saved, activity — combined across all of the account's devices. Both platforms display identical totals (X from Windows + Y from Android = X+Y everywhere).
3. **Custom dictionary words (accepted entries) and snippets synchronize.** Auto-learn *suggestions* never leave the device; only entries promoted into the dictionary/snippet collections sync.
4. **Transcription history never synchronizes.** It stays platform-local.
5. **Never synced:** API keys/secrets, provider & model selection, hotkeys, overlay/audio/IME preferences — anything platform-specific or secret.
6. **Restore semantics:** uninstall, lost phone, new laptop → sign in with the same Google account → dictionary, snippets, and lifetime statistics reappear. Industry-grade "it just works" behavior; zero conflict management visible to the user; user assumed non-technical.
7. **Efficiency:** no large uploads to Drive; computation local; only small consolidated state crosses the network.

Implicit requirements derived from these + first principles:

- R1. Concurrent offline edits on two devices must never corrupt, duplicate, or silently resurrect data.
- R2. Every failure mode (crash, network loss, token expiry, partial upload) must be recoverable by *doing nothing smarter than syncing again*.
- R3. Deletes must propagate and must not resurrect.
- R4. A device offline for months must reconcile correctly on return.
- R5. The remote state may be assumed complete-but-stale (single writer per device shard); it can never be partially missing *within* a file (see §6.4).
- R6. Clock skew may degrade which edit wins but must never corrupt data.
- R7. Schema evolution must not brick older app versions sharing the same account.

---

## 3. Proposed synchronization model

**"Per-device signed shards on Google Drive appDataFolder, merged deterministically on-device."**

- Each device owns exactly **one file** in the user's Drive `appDataFolder`: `fluence/devices/<device_id>.json` ("shard").
- A shard contains everything this device is authoritative for:
  - its **record set** (dictionary + snippets) as full records with modification metadata and tombstones,
  - its **statistics ledger** (lifetime counters + daily buckets).
- Devices **never write each other's shards.** A device reads all shards, merges deterministically, applies the result locally, and rewrites only its own shard.
- Merging is a **pure function**: `merge(allShards) -> state`. Same input ⇒ same output on every device. Convergence is guaranteed by construction, not by coordination.
- Statistics combine by **summation across shards** (each device contributes its own monotone ledger). Counting words twice is structurally impossible; losing counts requires losing a whole file.

This is a state-based LWW-element-set for records plus a summing counter set for stats — the two simplest convergent structures that exist, each applied exactly where its semantics fit.

There are **no cursors, no operation logs, no sequence numbers, no acks, no server logic.** The entire protocol is: LIST → GET → merge → apply → PUT own shard.

---

## 4. Why this is the simplest robust solution

The design was chosen against explicit alternatives (§19). The decisive reasoning:

1. **Transport is fixed to Google Drive** (product decision). Drive offers atomic per-file reads/writes, listing, and OAuth — but *no transactions across files, no server-side merge hook, no append-with-sequence-number*. Any design requiring cross-file atomicity or server arbitration is disqualified. That eliminates delta/cursor protocols and server-authoritative LWW in one stroke.
2. **Single-writer-per-file** is the only way to get correctness on a store without cross-file transactions. Two devices writing one shared document can lose updates during simultaneous syncs (traced concretely in §19, A2). Per-device shards make write conflicts structurally impossible rather than merely unlikely.
3. **State-based beats delta-based here.** Deltas save bytes and require cursors, ack tracking, dedup windows, and compaction. Our total dataset is tens of KB. Paying protocol complexity to save bytes we do not need to save is the definition of unjustified machinery. State-based sync is also *self-healing*: any interrupted or corrupted attempt is fully repaired by the next successful sync, because every sync recomputes truth from full state.
4. **Records want LWW-element-set; stats want summation.** Dictionary/snippets are tiny CRUD sets edited rarely; field-level merging (CRDT text merge) solves a problem this product doesn't have. Stats are monotone counters; summing per-device ledgers is conflict-free *by arithmetic*, not by resolution policy — there is nothing to resolve.
5. **Tombstones are unavoidable** (R3): without them, a delete on device A is undone when A's next merge sees the record alive in B's shard. With per-record `deleted` flags retained in shards, deletes converge like edits. Dataset size makes forever-retention free.

What complexity this model avoids entirely: change feeds, revision tables, sync-state machines beyond idle/syncing/error, retry queues, idempotency keys, clock synchronization, GC policies, device manifests, and any UI for conflicts.

---

## 5. Data model / schema

### 5.1 Canonical record envelope (wire format, inside every shard)

```json
{
  "v": 1,
  "device_id": "b0c7…-uuid-v4",
  "updated_at": 1724230000000,
  "records": [
    {
      "id": "uuid-v4",
      "col": "dict",
      "del": false,
      "mtime": 1724229000000,
      "origin": "device-uuid",
      "data": { "spoken": "torie", "corrected": "Tauri", "enabled": true }
    },
    {
      "id": "uuid-v4",
      "col": "snip",
      "del": false,
      "mtime": 1724229500000,
      "origin": "device-uuid",
      "data": { "trigger": "my linkedin", "expansion": "https://…" }
    },
    {
      "id": "uuid-v4",
      "col": "dict",
      "del": true,
      "mtime": 1724229800000,
      "origin": "device-uuid",
      "data": null
    }
  ],
  "stats": {
    "lifetime": { "words": 1284004, "chars": 7301222, "dictation_ms": 9044123, "sessions": 8211 },
    "daily": [
      { "day": "2026-08-20", "words": 5120, "dictation_ms": 31000 }
    ]
  }
}
```

Field notes:
- `col` ∈ {`dict`, `snip`} — canonical collections. Windows `dictionary.kind="correction"` ⇔ `dict`; Windows `dictionary.kind="expansion"` ⇔ `snip`; Android `custom_dictionary` ⇔ `dict`; Android Snippets ⇔ `snip`.
- `enabled` defaults `true` (serde/JSON defaults both sides). Android's per-entry toggle maps to it; Windows preserves it blindly and does not expose it.
- `mtime` = device wall-clock epoch ms at last local edit of this record. `origin` = editing device's id. Together they form the LWW total order.
- `stats.daily` is a rolling window (see 5.3); `lifetime` counters are cumulative and never reset.

### 5.2 Local schema changes required

**Windows**
- No ID migration needed (UUIDs already).
- New persistent stats ledger (see 5.3) because current stats derive from deletable history.
- Local stores keep **live records only**, unchanged schemas. Tombstones live exclusively in the local copy of the device's own shard (`sync/device_shard.json`) — sync memory is isolated from app storage.

**Android**
- Room migration v4→v5: add nullable `syncId TEXT` column to `custom_dictionary`; backfill all rows with fresh UUIDs on first sync enablement. Snippets JSON gains a string `syncId` per entry (same backfill).
- `stats_daily` continues as-is; it becomes the source for the shard's `stats` section.

### 5.3 Statistics ledger rules (both platforms)

- Incremented **transactionally at transcription-commit time**, independent of history retention. Clearing/deleting history never touches the ledger.
- Units canonicalized: `words` = whitespace-token count of final injected text; `chars`; `dictation_ms`; `sessions` (+1 per completed dictation).
- `daily` buckets keyed by device-local calendar day `YYYY-MM-DD`, pruned to a **400-day window** at write time (bounds file size; covers weekly + monthly views; lifetime numbers come from `lifetime` counters, so pruning loses nothing permanent).
- Shard `stats` section is rebuilt from the local ledger at every sync; the ledger itself is platform-native (Room table on Android; a small JSON/table on Windows).

### 5.4 What is deliberately absent from the schema

No revision numbers, no per-record server seq, no hashes (whole-record LWW needs none), no created_at, no per-collection files, no manifest, no device registry, no cursor fields.

---

## 6. Sync protocol / lifecycle

### 6.1 Triggers

| Trigger | Platform mechanism |
|---|---|
| Sign-in success | immediate sync |
| App start / resume | delayed sync (jittered ~15 s) |
| Local record CRUD | debounced sync (~3 s) |
| Stat increments | throttled sync (≥15 min since last) + best-effort on app exit |
| Stats screen opened | pull-only refresh if cache stale (>5 min) |
| Periodic safety net | WorkManager (Android, network-constrained) / low-frequency timer while running (Windows) |
| Manual "Sync now" | immediate |

All triggers funnel into one serialized sync job per device (mutex/actor). Concurrent invocations coalesce.

### 6.2 Sync algorithm (one pass)

```
SYNC():
  token = ensure_valid_oauth_token()            # refresh once on 401
  peers = list_and_download("fluence/devices/*.json", excluding own)
  merged_records = lww_merge(peers ∪ {own_local_shard})
  changes = diff(merged_records, local_stores)
  apply(changes)                                 # atomic locally (Room txn / tmp+rename)
  own_shard = build(merged_records_with_own_tombstones, current_local_ledger)
  validate(roundtrip_json(own_shard))            # refuse to upload unparseable state
  upload(own_shard → "fluence/devices/<device_id>.json")   # single PUT, replaces atomically
  persist local copy of own_shard; record last_sync; notify UI
```

Steps 4–6 are individually atomic; a crash between any two steps leaves a consistent local state and is repaired by the next run (state-based property).

### 6.3 First-time synchronization / restore (the "new phone" flow)

1. User signs in. Device generates `device_id` (random UUID v4, persisted).
2. Download all peer shards. If none exist → this is a fresh account: skip to 4.
3. Merge peer records → apply into local stores. Import respects each platform's existing dedup rules (canonical spoken/corrected key on Windows; unique `spokenText` on Android — colliding imports keep the existing row and adopt its identity).
4. Merge must also include **pre-existing local content**: local records enter the merge with fresh `mtime=now`, so a user's existing desktop dictionary flows *up* to the account on first sign-in rather than being clobbered.
5. Build own shard (merged records + tombstones inherited from peers + local ledger starting from current local stats) → upload.
6. UI shows combined stats immediately after step 2 (sum of downloaded ledgers + local).

Reinstall/new device repeats this identically. Old devices' shards remain and keep contributing their frozen ledgers — which is *correct*: those words were really dictated. Stale record sets are harmless because sets deduplicate by id (§18, item 8).

### 6.4 Consistency guarantees relied upon from Drive

- **File replacement is atomic w.r.t. readers:** a GET returns the complete previous or complete new content, never a torn file. (Drive media-content updates have this property.) This is what lets us assume "remote state is never partially missing within a file."
- **Listing is eventually consistent** at worst; a just-created peer shard may be invisible briefly. Effect: one extra sync cycle later. Harmless under state-based merging.

---

## 7. Conflict model

- **Granularity:** whole record. No field merging, no three-way merge, no content hashes.
- **Rule:** for each record id, winner = max `(mtime, origin)` lexicographic. Total order ⇒ deterministic ⇒ convergence on every device regardless of sync order.
- **Concurrent same-record edits** (rare: dictionary words are edited infrequently, usually from one device): newest edit wins silently. The loser's text is superseded — the standard, expected LWW behavior for preference-like data, and invisible to a non-technical user.
- **Cross-kind races** (A deletes while B edits): whichever event has the higher `(mtime, origin)` wins — a delete is just a tombstone record competing in the same order. Deterministic, no special-casing.
- **Clock skew caveat (accepted):** a device with a fast clock wins ties it might morally lose until its clock is corrected. This degrades *which valid version is shown*, never integrity, never duplication (R6). Documented tradeoff; the alternative (server-assigned sequence numbers) is unavailable on Drive and would reintroduce server infrastructure the product explicitly declined.

---

## 8. Delete model

- Deleting a record locally writes a **tombstone** into the own-shard record list: `{id, col, del: true, mtime, origin}`. The record is removed from the live local store simultaneously.
- Tombstones participate in LWW like any record. A tombstone with newer `(mtime, origin)` than a peer's live copy kills it on the next merge everywhere. Resurrection is impossible unless a peer *edits* the record later with a newer mtime — which is correct behavior (a genuine post-delete edit recreates it).
- Tombstones are retained **indefinitely** in shards. Cost: ~120 bytes each; dictionaries/snippets are human-curated (tens–hundreds of entries). A GC policy would be pure risk for zero measurable benefit. Explicitly omitted.
- Local stores never store tombstones; they live only in the shard (sync memory).

---

## 9. Retry / idempotency model

- **Idempotency by construction:** every sync is a full-state read-merge-write. Running it twice, or after a crash mid-run, produces the same result. There are no operations to deduplicate, hence no idempotency keys, no dedup windows, no replay protection.
- **Retries:** any failed sync simply remains pending; the next trigger retries. In-process transient failures (HTTP 5xx, socket reset) may retry immediately with short backoff (≤2 attempts) before returning to pending. WorkManager/timer provide the durable retry loop.
- **Upload-succeeds-response-lost:** the PUT landed; we think it failed. Next sync downloads our own shard like any peer-equivalent state and converges. No action needed.
- **Download-succeeds-commit-fails:** local apply is transactional; on failure nothing applied, retry later.
- **401 during sync:** refresh token once, retry the request once; else surface "sign-in required," stop, retry on next trigger.

---

## 10. Offline behavior

- Everything works offline exactly as today. Records and stats accumulate locally.
- Offline duration is irrelevant: there are no sessions to expire, no cursors to fall behind, no deltas to compact. A device offline for six months performs the identical LIST→GET→merge→PUT as one offline for six minutes (R4).
- Quota-bounded queueing: none needed. At most one pending sync intent (debounced/coalesced).
- Combined-stats display while offline shows local + last-known-peer sums, refreshed on next online sync.

---

## 11. Failure recovery model

The recovery story for *every* failure class is the same sentence: **the next successful sync repairs all state, because truth is recomputed from full state each time.**

Specifically:
- Crash mid-sync → local state consistent (atomic applies), remote state consistent (atomic PUT); rerun fixes any incompleteness.
- Network death mid-sync → same as crash.
- Corrupt/unparseable **peer** shard (bug-induced bad content; torn reads impossible per §6.4) → treat as empty, log, surface a warning badge, continue with remaining shards. Never let one bad file block the account.
- Corrupt **own** shard → prevented pre-upload by serialize→parse roundtrip validation; additionally the last-good local shard copy allows rebuilding.
- Drive outage/quota exhaustion → sync fails visibly, retries on triggers; local functionality unaffected.
- Token revoked/password changed → OAuth refresh fails → clear status "re-login required"; data intact locally; resyncs after re-auth.
- User signs into a **different Google account** on the same device → local sync cache (own shard incl. tombstones) is namespaced by account id and reset on switch; local user data merges into the new account on next sync (acceptable, documented).

---

## 12. Security model

- **Transport/storage:** all traffic TLS to Google; data resides in the user's own Drive `appDataFolder` — hidden from the user's normal Drive UI, ACL-scoped to the application, reachable only via the user's own OAuth grant. No Fluence-operated server ever exists; there is no third party beyond Google.
- **OAuth:** PKCE + system browser (Windows: loopback redirect via existing shell/open infrastructure; Android: Credential Manager / AuthorizationClient with `drive.appdata` + `profile` scopes — least privilege, app can only touch its own folder).
- **Token storage:** Windows Credential Manager (existing `credentials.rs` namespace pattern); Android EncryptedSharedPreferences (`androidx.security.crypto`, already a dependency). Tokens never logged, never synced.
- **Content sensitivity analysis (why no E2EE in v1):** synced payloads are dictionary words, snippet text, and aggregate counters. Transcripts, history, audio, and API keys are *structurally excluded* from the wire format. Passphrase-based E2EE would break the stated core UX ("log in on a new phone and everything is there") by introducing a forgotten-passphrase = total-loss failure mode, plus key-change/recovery flows — significant complexity against priority #4 to protect data classes already excluded. Revisit only if synced scope ever expands toward sensitive content; an AEAD layer over shard bodies is a contained future addition (encrypt `records`+`stats` blobs behind the same envelope) that does not alter the merge protocol.
- **Abuse containment:** payload size caps per shard (e.g., 1 MB hard limit, well above legitimate use); strict JSON schema validation on ingest; unknown fields ignored (§13).

---

## 13. Versioning / migration strategy

- Every shard carries `"v": N`. Readers must accept `v ≤ theirs` and **ignore unknown fields and unknown `col` values** (forward compatibility: an old app sharing the account with a new app neither crashes nor corrupts; it simply doesn't mirror collections it doesn't understand).
- Additive evolution (new fields inside `data`, new optional stat counters) requires no version bump.
- Breaking change ⇒ bump `v`, ship read-side migration in both apps during the overlap window; shards are rewritten lazily by each device on its next sync.
- Platform-local migrations stay platform-native (Room migration v4→v5 for `syncId`; Windows needs none).
- The canonical wire structs exist as one small shared spec (this document + mirrored test vectors, §16) rather than a shared library — two implementations, one golden-vector test suite, deliberately no shared runtime dependency.

---

## 14. Exact data flow

**Local edit (dictionary word added on Windows):**
```
UI → dictionary::add_dictionary_entry()
   → dictionary.json updated (atomic)
   → hook: upsert {id, col:"dict", del:false, mtime:now, origin:device} into local shard copy
   → signal sync scheduler (debounce 3 s)
```

**Sync pass (network):**
```
GET https://www.googleapis.com/drive/v3/files?q='appDataFolder' in parents + trashed=false
    → list of shard files (names contain device ids)
GET …/files/<fileId>?alt=media                      × N peers
merge (pure, in-memory)
apply diffs → dictionary.json / Room txn / snippets prefs
rebuild own shard from merged records + local stats ledger
PATCH/POST multipart media upload → fluence/devices/<device_id>.json   (single request)
```

**Stats increment (every completed dictation, both platforms):**
```
commit transcript → history insert (unchanged)
                  → ledger.upsert(day, +words, +ms); lifetime += …   [transactional]
                  → maybe schedule throttled sync
```

**Combined stats render:**
```
combined = Σ lifetime(peer shards) + lifetime(local ledger)
activity/day view = Σ daily buckets by calendar day across shards + local
cached; recomputed after each sync and on stats-screen open
```

---

## 15. Failure-mode analysis

| # | Scenario | Exact behavior |
|---|---|---|
| 1 | A adds word offline; B adds different word offline; both reconnect | Each merges union of both shards; both uploads contain both words. Both devices show both words. No loss possible — different ids, set union. |
| 2 | A and B edit the *same* word offline; reconnect | LWW on `(mtime, origin)` picks one deterministically; both converge to the winner. Loser's text superseded silently (documented LWW semantics). |
| 3 | A deletes word X while B edits X offline; reconnect | Delete-tombstone and edit compete in the same total order. If tombstone newer → X gone everywhere. If edit newer → X lives with new text everywhere. Deterministic either way. |
| 4 | Same sync retried multiple times (crash loops, double-trigger) | Full-state idempotent; repeated runs converge to identical result. No dupes (set-by-id), no double counting (sum over distinct device ledgers). |
| 5 | Upload succeeds, response lost (client thinks it failed) | Own shard actually updated; next sync treats it as ordinary state and proceeds. Zero corrective action. |
| 6 | Download succeeds, local commit fails (disk full, DB error) | Apply is transactional → nothing half-applied; sync returns to pending; retry later. Remote unaffected. |
| 7 | App crashes midway through sync | Interleaving of atomic local apply and atomic remote PUT leaves both sides consistent-prefix states; next sync completes the convergence. No locks held across crash (none exist). |
| 8 | Network disappears during sync | Partial GETs discarded wholesale (a shard is parsed only if fully received + schema-valid); PUT either happened or didn't. Retry on next trigger. |
| 9 | Auth expires during sync | Single refresh-and-retry; else job ends in "needs sign-in" state; auto-resumes after re-auth or next trigger. No queued-op corruption (there are no queued ops). |
| 10 | Two devices sync simultaneously | They write disjoint files (own shards only) → no lost update. Each may momentarily miss the other's freshest shard; both converge on the following trigger. Reads of a mid-replacement file are impossible (Drive atomicity). |
| 11 | Device offline for weeks/months | Identical code path as any sync (full state). Ledger still summed; stale records lose only to genuinely newer events. Tombstones it never saw still suppress deleted records? — Yes: suppression comes from *peer* shards' tombstones winning LWW, independent of the offline device's awareness. |
| 12 | Remote partially missing (e.g., a peer shard deleted in Drive web UI) | That device's contributions vanish from merge/stats until the device syncs again and re-uploads. Records reappear (they were only "deleted" by file absence — acceptable, self-healing; note: file absence ≠ tombstone, so this is the one path where a deleted record's *absence* isn't enforced — mitigated because the owning device re-uploads its tombstones on next sync). |
| 13 | Local DB contains stale sync metadata (e.g., restored old shard cache) | Worst case: extra merge inputs with older mtimes → they lose LWW; stats ledger is authoritative locally and merely re-uploaded. No corruption path; metadata is advisory, never authoritative. |
| 14 | Clock incorrect (ahead) on one device | Its records win ties until corrected; integrity/convergence unaffected (total order still holds — skew affects ordering semantics, not consistency). Behind-clock device: its edits lose until clock fixed. Accepted, documented. |
| 15 | Malformed/hostile JSON in a peer shard | Parse+schema validation fails → shard quarantined (treated empty, warning surfaced), rest of account unaffected; own shard roundtrip-validated before upload so corruption never propagates from us. |

---

## 16. Testing strategy

1. **Golden-vector merge tests (both platforms, identical fixtures):** shard sets → expected merged state. Fixtures shared as committed JSON so Rust and Kotlin assert byte-identical outcomes. Covers: union, LWW tiebreaks, tombstone-wins, tombstone-loses-to-edit, unknown-col skipping, v-tolerance, malformed input quarantine.
2. **Property tests (convergence):** randomized op sequences across k simulated devices → any interleaving of shard uploads → `merge(allShards)` equal on all devices. This is the theorem-as-test for the whole design.
3. **Ledger tests:** increment idempotence vs history-clear independence; day rollover; 400-day pruning; summation across arbitrary shard sets.
4. **Seam-mocked transport tests:** `CloudStore` trait (Rust) / interface (Kotlin) with in-memory fake Drive: success, 401→refresh→retry, 5xx, truncated body, missing files, simultaneous writers.
5. **Crash injection:** kill between apply and upload, between GET and apply (simulated via seam callbacks) → assert next-sync repair.
6. **Manual matrix (no CI dependence):** Win+Win, Win+Android, Android+Android; airplane-mode offline edits; reinstall-restore; account switch; clock skewed ±1 day; Drive file manually deleted.

---

## 17. Estimated implementation surface area

### Windows (Rust, `src-tauri`)
**New module `src-tauri/src/sync/`:**
- `mod.rs` — orchestrator: serialized sync job, triggers, debounce/throttle, status events (~150 LOC)
- `oauth.rs` — PKCE loopback flow, refresh, token persistence via `credentials.rs` (~200)
- `drive.rs` — Drive REST: list/get/upload appDataFolder via existing reqwest client (~150)
- `model.rs` — shard/record serde types + validation (~100)
- `merge.rs` — pure LWW merge + stats summation (~120)
- `stats_ledger.rs` — persistent per-day ledger + lifetime counters (~120)
- adapters: hooks in `dictionary.rs` / `snippets.rs` (bulk upsert/delete preserving ids + change signals), commit-path hook for ledger (~100 across files)

**Modified:** `main.rs` (module + commands + startup hook), `settings.rs` (account fields: signed-in email, sync enabled, last_sync — device-local settings), `Cargo.toml` (likely **zero** new crates; url/base64/sha2/reqwest/uuid/chrono present), `capabilities/default.json` (new commands), frontend `settings.js`/`index.html` (Account section UI).

### Android (Kotlin)
**New package `com.groq.voicetyper.sync/`:**
- `SyncManager.kt` — orchestrator (~180)
- `GoogleAuth.kt` — Credential Manager OAuth, token refresh (~120)
- `DriveClient.kt` — REST via existing OkHttp (~130)
- `Models.kt` + `Merger.kt` — wire types + pure merge (~200)
- `DeviceStatsSource.kt` — shard stats from `stats_daily` + lifetime (~80)
- `SyncWorker.kt` — WorkManager wrapper (~60)
- UI: Account section in Settings (~120)

**Modified:** `FluenceDatabase` (v4→v5 `syncId` migration), `CustomDictionaryEntry/Dao`, `SnippetPreferences` (syncId backfill + bulk import), repository change-signals, `build.gradle.kts` (play-services auth dependency).

### Totals
~1,000–1,300 LOC per platform including UI and tests. **Zero new backend infrastructure. One new Drive file per device. Two new local columns/ledgers. No new tables on Windows; one column migration on Android.**

---

## 18. Complexity audit — what we deliberately did NOT build

1. **Custom sync server / accounts database** — Drive OAuth *is* the account system; product chose it. Eliminates hosting, auth, ops, cost.
2. **Delta sync, cursors, sequence numbers, ack bookkeeping** — state-based full merge makes them unnecessary at our data scale; removing them removes the largest class of sync bugs (cursor corruption, gap detection, compaction).
3. **Operation logs / idempotency keys / dedup windows** — no operations exist to dedupe; full-state idempotence replaces them.
4. **Vector clocks, CRDT networks, MV-registers with user-facing resolution** — no requirement for concurrent-field merging; LWW-set suffices and needs 3 lines of ordering logic.
5. **Single shared document with LWW** — rejected for a *correctness* bug (simultaneous-sync lost update/resurrection, traced in §19/A2), not complexity.
6. **E2EE passphrase layer** — sensitive classes excluded from wire format; passphrase breaks restore UX and adds recovery machinery. Contained future option documented (§12).
7. **Real-time push (Drive change notifications, polling sockets)** — debounced pull matches the product's ambient rhythm; push infra buys seconds on data that changes a few times a week.
8. **Device registry / manifest / stale-shard GC** — unnecessary: record sets dedupe by id, and frozen ledgers from dead devices represent words genuinely spoken. Removing a lost device's contribution would actually be *wrong*. (Optional future UX: exclude-by-age toggle — explicitly out of scope.)
9. **Conflict-resolution UI** — target users are non-technical; silent deterministic LWW is the professional behavior requested.
10. **Settings synchronization** — deferred entirely. Every setting is either secret, platform-specific, or undesigned for cross-device semantics. The record mechanism generalizes to a `prefs` collection in one afternoon if a specific setting ever earns it.
11. **Separate stats files** — merged into the single per-device shard: fewer files, fewer GETs, one upload per sync.
12. **Hashes/content-addressing** — whole-record replacement needs no integrity comparison beyond LWW order.

Each omission traces to a requirement analysis showing zero user-visible loss; each inclusion (tombstones, per-device sharding, origin tiebreak, daily buckets, roundtrip validation) traces to a concrete failure mode in §15.

---

## 19. Alternatives considered and rejected

**A1. Whole-account single encrypted document, last-upload-wins.**
Simplest transport (one file). Fatal trace: A and B both download v5, both merge, both upload; second upload erases the first's merge. If B's deletion of Q lands last without a tombstone ever being seen by A, Q resurrects on A. Requires lock files or per-record metadata anyway → collapses into A2/A3 with extra steps. Rejected on correctness (priority #1).

**A2. Server-authoritative sync service (change log + seq + cursors).**
Best theoretical properties (clock-free total order, cheap deltas). Rejected: product mandated Drive/OAuth and no user-visible accounts; hosting contradicts constraint; deltas/cursors add the exact machinery §18.2 removes.

**A3. WebDAV/S3/Blob generic backend.**
Provider-agnostic, but: OAuth per provider multiplies auth code ×N providers; weaker atomicity guarantees vary by provider; more configuration UI for non-technical users. Drive-only is the requirement; abstraction to other backends later is cheap because `CloudStore` is already a 4-method seam (list/get/put/delete).

**A4. Field-level CRDTs (e.g., per-character or per-field merge).**
Justified only when the same artifact is concurrently edited by humans. Here artifacts are tiny discrete preference rows. Rejected: complexity with no product scenario.

**A5. Timestamp-free Lamport/HLC ordering.**
HLC still initializes from wall clocks (skew enters at init) and adds counter-persistence machinery per record. Server seq (the clean fix) is unavailable on Drive. Plain `(mtime, origin)` gives identical convergence guarantees with bounded, cosmetic skew effects. Rejected as machinery without a failure mode it fixes (§15.14).

**A6. Syncing history with E2EE "pro" tier.**
Directly contradicts stated product decision (history stays local). Not considered further.

---

## 20. Final recommended architecture

> **One Google account = one Drive appDataFolder containing one immutable-ownership shard per device.**
> Shards carry the device's dictionary/snippet records (with LWW metadata + tombstones) and its statistics ledger (lifetime counters + 400-day daily buckets).
> Sync = download peers → pure deterministic merge (LWW per record id; sum per stat) → atomically apply locally → atomically rewrite own shard.
> No servers, no cursors, no op logs, no clocks to trust, no conflicts to show, no state that the next sync cannot rebuild.

### Final simplification test (mandatory challenge)

*Can another piece be removed?*
- Tombstones → resurrection bug. Keep.
- Per-device shards → lost-update bug (A1). Keep.
- `origin` tiebreak → nondeterminism under simultaneous same-record edits. Keep (3 lines).
- Daily buckets → activity/monthly views impossible. Keep.
- Roundtrip validation before upload → corruption propagation. Keep (5 lines).
- Everything else proposed along the way was already cut (§18). **Nothing further can be removed without breaking a §15 failure mode.**

*Any hidden unrecoverable failure mode?*
One accepted, documented residual: deleting a device's shard file via Drive's own web UI temporarily un-counts that device and can transiently revive nothing (its tombstones return on its next sync) — self-healing, requires deliberate user action outside the app. No in-app failure mode exists that the next successful sync does not repair.

---

## Architectural summary (implementation-phase foundation)

1. **Storage:** Google Drive `appDataFolder`, path `fluence/devices/<device_id>.json`, one shard per device, JSON `v:1`.
2. **Shard contents:** `records[]` (id, col∈{dict,snip}, del, mtime, origin, data) + `stats{lifetime{words,chars,dictation_ms,sessions}, daily[{day,words,dictation_ms}]}`.
3. **Merge law:** records — max(`mtime`,`origin`) per id, tombstones retained forever; stats — sum across shards; both pure and order-independent.
4. **Protocol:** LIST → GET peers → merge → atomic local apply → validated atomic PUT of own shard. Serialized per device; triggered by login/start/edit-debounce/stat-throttle/manual/periodic.
5. **Identity:** `device_id` = random UUID persisted per install; account = Google account via OAuth (PKCE, `drive.appdata`); tokens in OS secure stores.
6. **Exclusions (hard):** history, audio, API keys, provider/model config, hotkeys/UI prefs, auto-learn suggestions.
7. **Platform deltas:** Android Room v4→v5 (`syncId` columns); Windows gains persistent stats ledger; both gain ~1 kLOC sync module + Account UI.
8. **Guarantees:** convergence always; no duplication ever; deletes never resurrect; any interruption repaired by next sync; skew affects only which valid value wins.
