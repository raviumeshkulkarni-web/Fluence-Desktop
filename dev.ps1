# dev.ps1 — Launch tauri dev with local Rust/MSVC toolchain
$ProjectRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$env:RUSTUP_HOME = "$ProjectRoot\.rust\rustup"
$env:CARGO_HOME  = "$ProjectRoot\.rust\cargo"
$env:PATH        = "$ProjectRoot\.rust\cargo\bin;$env:PATH"
& tauri dev