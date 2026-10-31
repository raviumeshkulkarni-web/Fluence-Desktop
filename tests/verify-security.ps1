# Fluence Security Changes - Frontend Verification Script
# Checks Change 3 (CSP), Change 4 (safe DOM), Change 5 (onclick), Change 7 (capabilities)
# Run: pwsh tests\verify-security.ps1

$ErrorActionPreference = "Stop"
$pass = 0
$fail = 0

function Test-Check {
    param([string]$Name, [bool]$Condition)
    if ($Condition) {
        Write-Host "  PASS  $Name" -ForegroundColor Green
        $script:pass++
    } else {
        Write-Host "  FAIL  $Name" -ForegroundColor Red
        $script:fail++
    }
}

# Rust `//` comment lines are stripped before every source grep below. Without
# this, a probe comment that shows the mutation one would make (and this
# audit's S5 probe table contains one) satisfies the regex as if it were live
# code. Defined once here so every section can use it.
function Remove-RustComments {
    param([string]$Source)
    ($Source -split "`n" | Where-Object { $_ -notmatch '^\s*//' }) -join "`n"
}

Write-Host "`n=== Change 7: shell:default removed from capabilities ===" -ForegroundColor Cyan
$capFiles = @("src-tauri/capabilities/main.json", "src-tauri/capabilities/overlay.json", "src-tauri/capabilities/wizard.json")
Test-Check "default.json removed (per-window capabilities)" (-not (Test-Path "src-tauri/capabilities/default.json"))
foreach ($f in $capFiles) {
    $cap = Get-Content $f -Raw
    $hasShell = $cap -match '"shell:default"'
    Test-Check "$f : shell:default NOT in capabilities" (-not $hasShell)
}
$overlay = Get-Content "src-tauri/capabilities/overlay.json" -Raw
Test-Check "overlay.json: no fs access" (-not ($overlay -match '"fs:'))
Test-Check "overlay.json: no updater access" (-not ($overlay -match '"updater:'))
Test-Check "overlay.json: no process restart" (-not ($overlay -match '"process:'))
$wizard = Get-Content "src-tauri/capabilities/wizard.json" -Raw
Test-Check "wizard.json: no fs access" (-not ($wizard -match '"fs:'))
Test-Check "wizard.json: no updater access" (-not ($wizard -match '"updater:'))
Test-Check "wizard.json: no process restart" (-not ($wizard -match '"process:'))

Write-Host "`n=== Change 3: CSP hardening in HTML files ===" -ForegroundColor Cyan
foreach ($file in @("src/index.html", "src/overlay.html", "src/wizard.html")) {
    $content = Get-Content $file -Raw
    # Extract just the CSP meta tag content (not full page HTML)
    $cspMatch = [regex]::Match($content, '<meta\s+[^>]*http-equiv="Content-Security-Policy"[^>]*content="([^"]*)"')
    if ($cspMatch.Success) {
        $csp = $cspMatch.Groups[1].Value
    } else {
        $csp = ""
    }
    $noStarScheme = -not ($csp -match 'script-src[^"]*https://\*')
    $noUnusedDomains = -not ($csp -match 'generativelanguage\.googleapis\.com')
    $noLocalhost = -not ($csp -match 'http://127\.0\.0\.1:1430')
    $noGroqApi = -not ($csp -match 'api\.groq\.com')
    $noOpenaiApi = -not ($csp -match 'api\.openai\.com')
    $noAnthropicApi = -not ($csp -match 'api\.anthropic\.com')
    Test-Check "${file}: no https://* wildcard in CSP" $noStarScheme
    Test-Check "${file}: no generativelanguage.googleapis.com in CSP" $noUnusedDomains
    Test-Check "${file}: no http://127.0.0.1:1430 in CSP" $noLocalhost
    Test-Check "${file}: no api.groq.com in CSP" $noGroqApi
    Test-Check "${file}: no api.openai.com in CSP" $noOpenaiApi
    Test-Check "${file}: no api.anthropic.com in CSP" $noAnthropicApi
}

Write-Host "`n=== Change 5: No inline onclick handlers in JS ===" -ForegroundColor Cyan
foreach ($file in @("src/js/settings.js")) {
    $content = Get-Content $file -Raw
    $noOnclick = -not ($content -match 'onclick\s*=')
    Test-Check "${file}: no onclick= attribute" $noOnclick
    $hasAddEventListener = $content -match 'addEventListener'
    Test-Check "${file}: uses addEventListener" $hasAddEventListener
}

Write-Host "`n=== Change 4: Safe DOM APIs (no innerHTML for model names) ===" -ForegroundColor Cyan
$settingsContent = Get-Content "src/js/settings.js" -Raw
$noModelInner = -not ($settingsContent -match 'modelSelect\.innerHTML')
Test-Check "settings.js: modelSelect.innerHTML removed" $noModelInner
$hasCreateEl = $settingsContent -match 'createElement'
Test-Check "settings.js: uses createElement for model names" $hasCreateEl

$wizardContent = Get-Content "src/js/wizard.js" -Raw
$noWizardInner = -not ($wizardContent -match '\.innerHTML\s*=')
Test-Check "wizard.js: no innerHTML assignments" $noWizardInner
$hasWizCreateEl = $wizardContent -match 'createElement'
Test-Check "wizard.js: uses createElement" $hasWizCreateEl

Write-Host "`n=== Change 1: sherpa-manifest.json exists ===" -ForegroundColor Cyan
$manifestExists = Test-Path "src-tauri/sherpa-manifest.json"
Test-Check "sherpa-manifest.json exists" $manifestExists
if ($manifestExists) {
    $m = Get-Content "src-tauri/sherpa-manifest.json" -Raw | ConvertFrom-Json
    Test-Check "manifest has sherpa_version" ($null -ne $m.sherpa_version -and $m.sherpa_version -ne "")
    Test-Check "manifest has 3 downloads" ($m.downloads.Count -eq 3)
    Test-Check "manifest has expected_binaries" ($null -ne $m.expected_binaries -and $m.expected_binaries.PSObject.Properties.Count -gt 0)
}

Write-Host "`n=== Change 6: URL validation deps in Cargo.toml ===" -ForegroundColor Cyan
$cargo = Get-Content "src-tauri/Cargo.toml" -Raw
Test-Check "url crate in Cargo.toml" ($cargo -match 'url\s*=\s*"2')
Test-Check "sha2 crate in Cargo.toml" ($cargo -match 'sha2\s*=\s*"0\.10"')
Test-Check "hex crate in Cargo.toml" ($cargo -match 'hex\s*=\s*"0\.4"')

Write-Host "`n=== Regression fixes: missing-key path, target parity, error mapping ===" -ForegroundColor Cyan
$agentRs = Get-Content "src-tauri/src/agent.rs" -Raw
Test-Check "secure agent path uses get_llm_api_key_or_err (friendly missing-key)" ($agentRs -match 'get_llm_api_key_or_err\(&preset\)')
Test-Check "secure agent path does not call read_api_key_target directly" (-not ($agentRs -match 'read_api_key_target\(&target\)'))
$credRs = Get-Content "src-tauri/src/credentials.rs" -Raw
Test-Check "credentials.rs documents canonical slug contract" ($credRs -match 'FIX-02 contract')
Test-Check "validator still rejects hyphenated targets (strict namespace)" ($credRs -match 'groq-key')
$providersTs = Get-Content "web/src/ipc/providers.ts" -Raw
Test-Check "providers.ts keyTarget uses canonicalPresetSlug" (($providersTs -match 'function canonicalPresetSlug') -and ($providersTs -match 'canonicalPresetSlug\(preset\)'))
Test-Check "providers.ts maps non-alphanumerics to underscore" ($providersTs -match '\[\^a-z0-9_\]/g')
$vanillaSettings = Get-Content "src/js/settings.js" -Raw
Test-Check "settings.js defines canonicalPresetSlug + credentialTarget" (($vanillaSettings -match 'function canonicalPresetSlug\(preset\)') -and ($vanillaSettings -match 'function credentialTarget\(type, preset\)'))
Test-Check "settings.js has no inline ApiKey target building left" (-not ($vanillaSettings -match 'ApiKey/\$\{'))
$overlayJs = Get-Content "src/js/overlay.js" -Raw
Test-Check "overlay maps unauthorized window as non-retryable" ($overlayJs -match "Not allowed from this window")
Test-Check "overlay maps credential-target denial as non-retryable" ($overlayJs -match "unknown credential target")
Test-Check "overlay maps raw credential-store errors as non-retryable" ($overlayJs -match "CredReadW")
Test-Check "overlay maps oversized input as non-retryable" ($overlayJs -match "exceeds maximum length")
Test-Check "overlay keeps Missing LLM key non-retryable mapping" ($overlayJs -match "Missing LLM key', retryable: false")

# --- Audit S1/S8: sync credential surface ---------------------------------
# The sync remediation added ACL gates on snippets/suggestion/history commands
# and a test-only credential write/delete seam. Neither had any coverage here:
# the gates were validated only by Rust tests, and the seams are #[cfg(test)]-
# only so they cannot fire in production - but nothing mechanically stopped a
# future edit from dropping the gate. These are cheap static greps that fail
# loudly if either regression reappears.

Write-Host "`n=== S8: sync command ACL gates (S1) ===" -ForegroundColor Cyan

# Per-command, not a whole-file grep. An earlier version of this check only asked
# "does scheduler.rs contain require_caller at all", which stayed green when a
# single gate was deleted - the exact failure the check exists to catch.
#
# Only the sync-identity commands are asserted. Other commands in these files
# (local history reads/writes, for instance) are intentionally ungated and a
# blanket parity assertion would be false.
$gatedCommands = @{
    "src-tauri/src/sync/scheduler.rs"   = @("sync_sign_in", "sync_sign_out", "sync_toggle", "sync_run_now", "sync_get_status", "get_account_stats")
    "src-tauri/src/snippets.rs"  = @("get_snippets", "set_snippets_enabled", "add_snippet", "update_snippet", "delete_snippet")
    "src-tauri/src/suggestion.rs" = @("get_suggestions", "accept_suggestion_command", "dismiss_suggestion_command", "clear_dismissed_suggestions_command", "get_dismissed_count", "expire_stale_suggestions_command", "get_learning_suggestions", "get_auto_accepted_suggestions", "revert_auto_accepted_suggestion")
    "src-tauri/src/history.rs"          = @("delete_history_entry", "clear_history", "get_history_stats", "get_weekly_activity")
    "src-tauri/src/sync/stores.rs"      = @("get_account_activity")
}
foreach ($path in $gatedCommands.Keys) {
    if (-not (Test-Path $path)) { continue }
    $lines = Get-Content $path
    foreach ($cmd in $gatedCommands[$path]) {
        $found = $false
        for ($i = 0; $i -lt $lines.Count; $i++) {
            if ($lines[$i] -match ('fn\s+' + [regex]::Escape($cmd) + '\s*\(')) {
                $gated = $false
                for ($k = [Math]::Max(0, $i - 6); $k -lt [Math]::Min($lines.Count, $i + 14); $k++) {
                    if ($lines[$k] -match 'require_caller') { $gated = $true; break }
                }
                $found = $true
                Test-Check "$cmd carries an ACL gate" $gated
                break
            }
        }
        # Absence from the file at all is itself a finding: the command was either
        # renamed or lost, and the gate guarantee went with it.
        Test-Check "$cmd still exists in $(Split-Path $path -Leaf)" $found
    }
}

# N6 completeness cross-check (reviewer adf6ed5e): the table above is only as
# complete as its last edit. A new gated command that nobody adds here would be
# silently unchecked. So invert the scan WITHIN THE TABLE'S FILES: every
# `#[tauri::command]` function that declares a `window` param must either be in
# the table (gated) or in the explicit allowlist below (intentionally ungated,
# with the reason stated). Anything else is a new security-relevant command
# that must be triaged, not silently passed.
#
# Scoped to the table's files on purpose: `window: tauri::Window` is a common
# Tauri pattern across the whole tree (settings, transcription, downloads),
# and those commands were never in the sync-audit scope. A tree-wide check
# would be scope creep and would fail on ~50 unrelated commands.
$windowAllowlist = @{
    # Local history writes are open to the overlay by design (see acl.rs:
    # "get/save_history | main,overlay | open"). No identity is asserted, so no
    # gate belongs here - and adding one would break the overlay save path.
    "save_history_entry" = "overlay-open by design, no identity asserted"
}
$windowCmds = @{}
foreach ($path in $gatedCommands.Keys) {
    if (-not (Test-Path $path)) { continue }
    $src = ((Get-Content $path -Raw) -split "`n" | Where-Object { $_ -notmatch '^\s*//' }) -join "`n"
    foreach ($m in [regex]::Matches($src, '#\[tauri::command\]')) {
        # The signature follows the attribute. Capture the parameter list up to
        # its closing paren (no nested parens occur in these signatures), then
        # require the window param inside THAT - not anywhere later in the file.
        $ahead = $src.Substring($m.Index, [Math]::Min(1200, $src.Length - $m.Index))
        $sig = [regex]::Match($ahead, '(?s)fn\s+(\w+)\s*\(([^)]{0,1200})\)')
        if ($sig.Success -and $sig.Groups[2].Value -match 'window:\s*tauri::Window') {
            $windowCmds[$sig.Groups[1].Value] = $path
        }
    }
}
$tableEntries = @{}
foreach ($path in $gatedCommands.Keys) { foreach ($cmd in $gatedCommands[$path]) { $tableEntries[$cmd] = $path } }
foreach ($name in ($windowCmds.Keys | Sort-Object)) {
    $tracked = $tableEntries.ContainsKey($name)
    $allowed = $windowAllowlist.ContainsKey($name)
    Test-Check "window-taking command $name in a sync file is tracked or explicitly allowed" ($tracked -or $allowed)
}

Write-Host "`n=== S8: sync transport hardening (S2/S3/S4) ===" -ForegroundColor Cyan

# These properties are proven behaviorally by Rust tests; these static checks
# make them fail loudly here too if a future edit removes the mechanism. Each
# is mutation-verified alongside its Rust counterpart.
$schedSrc = Remove-RustComments (Get-Content "src-tauri/src/sync/scheduler.rs" -Raw)
$httpSrc = Remove-RustComments (Get-Content "src-tauri/src/http_client.rs" -Raw)
$driveSrc = Remove-RustComments (Get-Content "src-tauri/src/sync/drive.rs" -Raw)
$authSrc = Remove-RustComments (Get-Content "src-tauri/src/sync/auth.rs" -Raw)

# S2: sync-status emissions are scoped to the main window through one helper.
# An app-level broadcast would leak per-account status (including the raw
# account key) to every webview including overlay/wizard. The one legitimate
# emit is `win.emit(...)` inside the helper, where `win` comes from
# `get_webview_window` — so the check asserts exactly one emit of the event
# and that no `app`-level emit of it exists, rather than forbidding the
# helper's own call.
Test-Check "S2: single sync-status emit helper exists" ($schedSrc -match 'fn emit_status_to_main')
Test-Check "S2: helper resolves the main window by label" ($schedSrc -match 'get_webview_window\(crate::acl::MAIN_WINDOW\)')
Test-Check "S2: exactly one emit of the status event" (([regex]::Matches($schedSrc, '\.emit\(SYNC_STATUS_EVENT')).Count -eq 1)
Test-Check "S2: no app-level sync-status broadcast" (($schedSrc -notmatch 'app\.emit\(SYNC_STATUS_EVENT') -and ($schedSrc -notmatch '\.emit\("sync-status"'))
Test-Check "S2: status errors are sanitized before emit" ($schedSrc -match 'fn sanitize_status_error')

# S3: redirect policy. The shared client follows redirects only through an
# explicit allow-policy; the bearer-auth Drive client follows none, so a
# redirect cannot smuggle the Authorization header to another host.
Test-Check "S3: shared client uses a custom redirect policy" ($httpSrc -match 'Policy::custom')
Test-Check "S3: custom policy gates on redirect_is_allowed" ($httpSrc -match 'redirect_is_allowed')
Test-Check "S3: Drive bearer client follows no redirects" ($driveSrc -match 'Policy::none\(\)')

# S4: the loopback accept loop is byte-capped. `push_redirect_head` does not
# clip, so `take = room.min(..)` is the only ceiling; a read sized without the
# room removes it silently.
Test-Check "S4: byte cap constant is 4096" ($authSrc -match 'REDIRECT_HEAD_MAX_BYTES:\s*usize\s*=\s*4096')
Test-Check "S4: accept loop sizes reads by remaining room" ($authSrc -match 'let take = room\.min\(chunk\.len\(\)\)')
Test-Check "S4: accept loop breaks when the room is exhausted" ($authSrc -match 'if room == 0')

Write-Host "`n=== S8: network/destructive e2e stays out of the default suite ===" -ForegroundColor Cyan

# `e2e_delete_and_redownload_with_integrity` deletes the real model directory
# and downloads 100MB+ over the network. It failed intermittently on a network
# read with its file byte-identical to HEAD — environmental, pre-existing, and
# unrelated to any revision. The deliberate policy (reviewer-concurred) is: it
# stays runnable on demand but NEVER in the default `cargo test` run. These
# checks fail loudly if a future edit returns it to the suite.
$dlSrc = Remove-RustComments (Get-Content "src-tauri/src/offline_downloader.rs" -Raw)
Test-Check "e2e download test is #[ignore]d out of the default suite" ($dlSrc -match '#\[ignore\][\s\S]{0,300}?async fn e2e_delete_and_redownload_with_integrity')
Test-Check "e2e download test additionally requires FLUENCE_RUN_OFFLINE_E2E=1" ($dlSrc -match 'FLUENCE_RUN_OFFLINE_E2E')
Test-Check "e2e gate is checked before any deletion" (
    ($dlSrc -match '(?s)async fn e2e_delete_and_redownload_with_integrity.*?FLUENCE_RUN_OFFLINE_E2E.*?delete_model_files') -and
    ($dlSrc -notmatch '(?s)async fn e2e_delete_and_redownload_with_integrity.*?delete_model_files\(\)[\s\S]{0,200}?FLUENCE_RUN_OFFLINE_E2E')
)

Write-Host "`n=== S8: credential test seams cannot fire in production ===" -ForegroundColor Cyan

$credRs2 = Remove-RustComments (Get-Content "src-tauri/src/credentials.rs" -Raw)
$schedCode = Remove-RustComments (Get-Content "src-tauri/src/sync/scheduler.rs" -Raw)
Test-Check "read seam is cfg(test) gated" ($credRs2 -match '#\[cfg\(test\)\]\s*\r?\nthread_local! \{[\s\S]*?TEST_SYNC_TOKEN_READ')
Test-Check "write seam is cfg(test) gated" ($credRs2 -match '#\[cfg\(test\)\]\s*\r?\nthread_local! \{[\s\S]*?TEST_SYNC_TOKEN_WRITE')
Test-Check "delete seam is cfg(test) gated" ($credRs2 -match '#\[cfg\(test\)\]\s*\r?\nthread_local! \{[\s\S]*?TEST_SYNC_TOKEN_DELETE')

# The deletion path is exactly two deep and sign-out is the only way in:
#   sync_sign_out (scheduler.rs) -> AuthSession::forget_refresh_token (auth.rs)
#     -> credentials::delete_sync_refresh_token (credentials.rs, defined once)
# Anything that shortens that chain would let a recovery path delete a credential,
# which is the S5-d property. Asserted on call SITES, not file counts, because a
# second call inside either function would otherwise be invisible.
$credDefs = ([regex]::Matches($credRs2, 'fn delete_sync_refresh_token')).Count
Test-Check "credentials.rs defines delete_sync_refresh_token exactly once" ($credDefs -eq 1)

$authCode = Remove-RustComments (Get-Content "src-tauri/src/sync/auth.rs" -Raw)
$authCalls = ([regex]::Matches($authCode, 'delete_sync_refresh_token\(\)')).Count
Test-Check "auth.rs calls it exactly once, inside the AuthSession wrapper" ($authCalls -eq 1)
Test-Check "the wrapper is the AuthSession forget method" ($authCode -match '(?s)pub fn forget_refresh_token[\s\S]{0,200}?delete_sync_refresh_token\(\)')

$schedCalls = ([regex]::Matches($schedCode, 'forget_refresh_token\(\)')).Count
Test-Check "scheduler.rs reaches the credential only through the wrapper" ($schedCalls -eq 1)
Test-Check "and that one call site is sync_sign_out" ($schedCode -match '(?s)pub fn sync_sign_out[\s\S]{0,900}?forget_refresh_token\(\)')

# Isolate just the reconcile function body: from its signature to the next
# top-level `fn ` / `#[` after it.
$reconcile = [regex]::Match($schedCode, '(?s)fn reconcile_persisted_account_key.*?(?=\n    (?:pub )?(?:async )?fn |\n    #\[|\n\})')
Test-Check "reconcile function body isolated for the S5-d check" ($reconcile.Success)
Test-Check "reconcile never deletes the credential (S5-d)" ($reconcile.Value -notmatch 'delete_sync_refresh_token|forget_refresh_token')

Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Results: $pass passed, $fail failed" -ForegroundColor $(if ($fail -eq 0) { "Green" } else { "Red" })
if ($fail -gt 0) { exit 1 }
