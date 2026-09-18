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

Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Results: $pass passed, $fail failed" -ForegroundColor $(if ($fail -eq 0) { "Green" } else { "Red" })
if ($fail -gt 0) { exit 1 }
