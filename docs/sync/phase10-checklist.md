# Phase 10 — Release Readiness Checklist (Windows client)

Status: **draft for execution**. The Windows client code is complete through Phase 9
(quarantine UX + layer-6 failure-injection, 307 lib tests). This checklist is for the
human to run **after** the Android client reaches Phase 8 and a live Google Drive
`client_secret` is available for the Windows release.

> Mirroring note: this file lives only in the **Windows** repo. The Android repo mirrors
> only `spec.md`, `plan.md`, and `experiments.md`. Do not copy this checklist to Android.

---

## 0. Preconditions (all must be true before S1–S6)

- [ ] Windows app built and installed from the release bundle
      (`Fluence_1.15.0_x64-setup.exe` or `.msi` from `src-tauri/target/release/bundle/`).
- [ ] Windows release was signed OR run unsigned with the updater gated off (see §4).
- [ ] `FLUENCE_SYNC_CLIENT_SECRET` env var set for the Windows user **or** a valid
      `sync-oauth.json` placed at `%LOCALAPPDATA%\Fluence\sync-oauth.json`
      (`{ "client_secret": "..." }`). Verify by clicking **Sign in with Google** and
      completing the OAuth loop — you should see your email under *Account*.
- [ ] Android client (Phase 8) is installed and signed in to Drive with the **same**
      Google account.
- [ ] Android OAuth client ID is wired into the Android build (separate client ID from
      the Windows `236666538373-005rdohmcf6cgh0in10v5v8nhcc1m85k.apps.googleusercontent.com`).
- [ ] Both clients are on the same Wi-Fi / network and Drive sync is **Enabled** on both.

How to read the scenarios: each S-step lists a **do** action and the **expected** result
on the **Windows** side and the **Android** side. The Windows app uses a Google Drive
folder it owns (per-account namespace); Android writes the same record files there, so
each client should converge to the same content.

---

## 1. Acceptance scenarios (spec §27 Phase 10, S1–S6)

### S1 — First sync upload (Windows → Drive)
- **Do**: On Windows, ensure there is at least one history transcription, one dictionary
  correction, one snippet, and one settings change. Open Sync, click **Sync now**.
- **Expected (Windows)**: *Sync status* shows "Syncing right now…" then "Last synced
  <time>", no error. The Drive folder now contains `*.json` files named by record UUID.
- **Expected (Android)**: After its next background pass (or manual sync), the same
  records appear in the Android list with identical text. No duplicates.

### S2 — Cross-client import (Android → Windows)
- **Do**: Create a new record on Android (e.g. a dictionary correction). Trigger Android
  sync. On Windows, click **Sync now** (or wait for the 5-min cadence).
- **Expected (Windows)**: The Android-created record appears in the Windows UI with the
  same content. The quarantine list stays empty.
- **Expected (Android)**: Record remains present and marked synced.

### S3 — Edit convergence (same record edited on both)
- **Do**: Edit a dictionary correction both on Windows and Android (different `corrected`
  text) while offline-ish, then sync both.
- **Expected (Windows)**: Content-deviation is detected; the conflicting record is placed
  in **Quarantine** (reason "Content differs from the synced copy") rather than silently
  overwritten. Same on Android.
- **Expected (Android)**: Mirrors Windows — both clients surface the conflict for the
  user, neither clobbers the other.

### S4 — Delete propagation
- **Do**: On Windows, delete a synced snippet. Click **Sync now**.
- **Expected (Windows)**: The Drive file is tombstoned (the record still exists locally
  marked deleted; remote file carries `deleted_at`).
- **Expected (Android)**: After its next pass, the snippet is removed from the Android
  list (tombstone-wins). The remote file is not physically deleted.

### S5 — Offline resilience / retry
- **Do**: With Wi-Fi off, click **Sync now** repeatedly, then restore Wi-Fi and sync.
- **Expected (Windows)**: Transient failures surface as `Retryable` (backoff shown in
  status after a few attempts), nothing is lost, and once online the pass completes and
  reaches the fixed point. The single-flight latch never wedges (no "stuck syncing").
- **Expected (Android)**: Same graceful retry behavior.

### S6 — Sign-out / re-auth
- **Do**: Click **Sign out** on Windows, then **Sign in with Google** again (re-auth).
- **Expected (Windows)**: Status returns to signed-out then signed-in; existing Drive data
  is retained; a fresh pass re-validates and resumes. A 401 mid-pass surfaces
  "authentication required — sign in again" and pauses auto-scheduling until re-auth.

---

## 2. Experiment 1 completion (cross-account conflict)

Exp 1 verified the Windows side alone (create `000A.json`, cross-account isolation). To
close it end-to-end with Android:

- [ ] On Android, create a record whose file name is `000A.json` (the experiment fixture
      UUID) with distinct content.
- [ ] Sync Android, then sync Windows. Confirm Windows imports/merges `000A.json` and the
      experiment's cross-account check still holds: a file stamped to a *different*
      account is **never** imported, deleted, or modified by this account.
- [ ] Confirm no `collision`/`id_name_mismatch` quarantine appears for legitimate same-
      account duplicates; only true conflicts quarantine.

---

## 3. Quarantine manual walkthrough

- [ ] **Seed a corrupt file**: place a junk file (e.g. `00000000-0000-4000-8000-0000000000ff.json`
      containing `this is not json {`) into the app's Drive-synced folder via the Drive web
      UI or by stopping the app, editing a local record file, and reopening.
- [ ] Open **Settings → Sync**. Confirm the record appears under **Quarantined records**
      with reason **"Corrupt file"** and a *placeholder* badge.
- [ ] Click **Restore** on it. Confirm the toast and that the next sync re-evaluates it
      (if still corrupt, it re-quarantines; if now valid, it imports).
- [ ] Re-seed the corrupt file, then click **Discard**. Confirm a `window.confirm` prompt
      appears; accepting removes the local row. Confirm the Drive file is **never** deleted
      by the discard, and that if the synced copy still conflicts the next pass re-quarantines.
- [ ] Empty state: with no quarantined records, confirm "Nothing quarantined — all synced
      records are healthy." is shown.

---

## 4. Release gates

- [ ] **Version bump**: `tauri.conf.json`, `package.json`, and `Cargo.toml` are bumped
      from `1.14.1` to `1.15.0` with release notes (`docs/release-notes-v1.15.0.md`) *before* tagging.
- [ ] **Updater signing**: set `TAURI_SIGNING_PRIVATE_KEY` (and
      `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` if the key is encrypted) in the release
      build environment. **The private key must NEVER be committed to version control.**
      Without it, `npm run build` produces the MSI/NSIS installers but fails
      to sign the updater `.zip` artifacts (no `.sig` signature sidecar), so in-place updates
      via the `latest.json` endpoint will reject the package.
- [ ] **Updater endpoint & manifest**: confirm `latest.json` is generated (with version,
      release notes summary, and signature strings) and published alongside the signed
      bundle artifacts at `https://github.com/raviumeshkulkarni-web/Fluence-Desktop/releases/latest/download/latest.json`.
- [ ] **Android release OAuth client note**: this Windows build uses client ID
      `236666538373-005rdohmcf6cgh0in10v5v8nhcc1m85k.apps.googleusercontent.com`; the
      Android release must use its own client ID, and both must be authorized for the same
      Drive API project / redirect semantics.
- [ ] **Bundle smoke test**: install the fresh MSI and NSIS bundles on a clean Windows
      machine; verify launch, global hotkeys, recording→transcription, offline model
      download, and a full sync round-trip (S1–S6) before publishing.
- [ ] **Deprecation note**: the config uses `bundle.createUpdaterArtifacts: "v1Compatible"`,
      which Tauri v3 will remove. Plan a migration to `true` once users are on the v2
      updater plugin — not blocking for this release.

---

## 5. Known limitations / out of scope (Windows, this phase)

- Live end-to-end Drive sync (S1–S6, Exp 1 cross-account) is **unverified** pending the
  user's `client_secret` and Android Phase 8.
- Quarantine UI + layer-6 failure-injection are code-complete and unit-tested (307 lib
  tests) but the manual quarantine walkthrough (§3) needs a real Drive folder or a
  stopped-app file edit to seed a corrupt record.
- No secret is committed; the runtime `client_secret` is read from env or
  `%LOCALAPPDATA%\Fluence\sync-oauth.json` only.
