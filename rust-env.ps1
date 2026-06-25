# rust-env.ps1 - Project-local Rust + MSVC toolchain helper
# Usage:
#   . .\rust-env.ps1               dot-source to activate in current shell
#   .\rust-env.ps1 cargo check     run a single command

$ProjectRoot = Split-Path -Parent $MyInvocation.MyCommand.Path

# --- 1. Local Rust toolchain ---
$env:RUSTUP_HOME = "$ProjectRoot\.rust\rustup"
$env:CARGO_HOME  = "$ProjectRoot\.rust\cargo"
$env:PATH        = "$ProjectRoot\.rust\cargo\bin;$env:PATH"

# --- 2. Local MSVC Build Tools ---
$msvcRoot = "$ProjectRoot\.msvc"

# Find the MSVC compiler version directory
$msvcToolsBase = "$msvcRoot\VC\Tools\MSVC"
if (Test-Path $msvcToolsBase) {
    $msvcVersion = Get-ChildItem $msvcToolsBase | Sort-Object Name -Descending | Select-Object -First 1
    if ($msvcVersion) {
        $msvcBin = "$($msvcVersion.FullName)\bin\Hostx64\x64"
        $env:PATH = "$msvcBin;$env:PATH"
    }
}

# Find the Windows SDK (local first, then system fallback)
$sdkRoot = "$msvcRoot\Windows Kits\10"
$systemSdkRoot = "C:\Program Files (x86)\Windows Kits\10"
$useSdkRoot = $null

if (Test-Path "$sdkRoot\bin") {
    $useSdkRoot = $sdkRoot
} elseif (Test-Path "$systemSdkRoot\bin") {
    $useSdkRoot = $systemSdkRoot
}

if ($useSdkRoot) {
    $sdkVersion = Get-ChildItem "$useSdkRoot\bin" -Filter "10.*" | Sort-Object Name -Descending | Select-Object -First 1
    if ($sdkVersion) {
        $env:PATH = "$($sdkVersion.FullName)\x64;$env:PATH"
    }
}

# Set LIB and INCLUDE for the MSVC linker
if ($msvcVersion) {
    $env:LIB     = "$($msvcVersion.FullName)\lib\x64"
    $env:INCLUDE = "$($msvcVersion.FullName)\include"
}

$sdkLibRoot = $null
if ($useSdkRoot) {
    $sdkLibCheck = Test-Path "$useSdkRoot\Lib"
    if ($sdkLibCheck) {
        $sdkLibRoot = $useSdkRoot
    }
}
if (-not $sdkLibRoot) {
    $systemLibCheck = Test-Path "$systemSdkRoot\Lib"
    if ($systemLibCheck) {
        $sdkLibRoot = $systemSdkRoot
    }
}

if ($sdkLibRoot) {
    $sdkLibVersion = Get-ChildItem "$sdkLibRoot\Lib" -Filter "10.*" | Sort-Object Name -Descending | Select-Object -First 1
    if ($sdkLibVersion) {
        $env:LIB     = "$env:LIB;$($sdkLibVersion.FullName)\um\x64;$($sdkLibVersion.FullName)\ucrt\x64"
        $env:INCLUDE = "$env:INCLUDE;$sdkLibRoot\Include\$($sdkLibVersion.Name)\um;$sdkLibRoot\Include\$($sdkLibVersion.Name)\ucrt;$sdkLibRoot\Include\$($sdkLibVersion.Name)\shared"
    }
}

if ($args.Count -gt 0) {
    & $args[0] $args[1..($args.Count - 1)]
} else {
    Write-Host "Local Rust + MSVC env activated."
    Write-Host "  RUSTUP_HOME = $env:RUSTUP_HOME"
    Write-Host "  CARGO_HOME  = $env:CARGO_HOME"
    Write-Host "  MSVC tools  = $msvcBin"
}