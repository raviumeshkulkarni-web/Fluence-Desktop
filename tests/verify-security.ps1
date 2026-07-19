# Fluence Security Changes — Frontend Verification Script
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
$cap = Get-Content "src-tauri/capabilities/default.json" -Raw
$hasShell = $cap -match '"shell:default"'
Test-Check "shell:default NOT in capabilities" (-not $hasShell)

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

Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Results: $pass passed, $fail failed" -ForegroundColor $(if ($fail -eq 0) { "Green" } else { "Red" })
if ($fail -gt 0) { exit 1 }
