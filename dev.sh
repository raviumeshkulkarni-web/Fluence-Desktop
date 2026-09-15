#!/usr/bin/env sh
# dev.sh - Launch tauri dev on Linux (counterpart to dev.ps1 on Windows).
# Uses the system Rust toolchain (rustup/cargo on PATH). No local
# RUSTUP_HOME/CARGO_HOME override: Linux contributors manage toolchains
# via rustup directly.
set -eu
exec npx tauri dev "$@"
