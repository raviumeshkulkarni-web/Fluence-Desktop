# Contributing to Fluence

Thank you for your interest in Fluence! Contributions of all sizes are welcome, including bug reports, feature ideas, documentation improvements, design feedback, and pull requests - all help make the project better. No contribution is too small - fixing typos, improving documentation, reporting bugs, and suggesting improvements are all valuable.

---

## Project Philosophy

Fluence is built around a few core principles:

- Privacy first
- Local-first experience whenever possible
- Reliability over feature count
- Production-quality user experience and reliability
- Simple, maintainable architecture

When proposing changes, please try to preserve these principles.

Fluence aims to remain focused and lightweight. New features should align with the project's core purpose rather than expanding it into a general productivity platform.

---

## Table of Contents

- [Reporting Bugs](#reporting-bugs)
- [Suggesting Features](#suggesting-features)
- [Development Setup](#development-setup)
- [Project Structure](#project-structure)
- [Coding Guidelines](#coding-guidelines)
- [Making Changes](#making-changes)
- [Pull Request Process](#pull-request-process)
- [Community](#community)
- [Good First Issues](#good-first-issues)

---

## Reporting Bugs

If you find a bug, please open a GitHub issue with the following information:

**Bug Report Template:**

```markdown
**Description:**
A clear description of the bug.

**Steps to Reproduce:**
1. Open Fluence
2. Do '...'
3. See error

**Expected Behavior:**
What you expected to happen.

**Actual Behavior:**
What actually happened.

**Environment:**
- OS: Windows 10/11 (version)
- Fluence Version:
- Audio Input Device:
```

---

## Suggesting Features

Feature ideas are welcome! Please open an issue with:

- **What problem does this solve?**
- **How should it work?**
- **Any mockups or examples?**

Before implementing a large feature, please open an issue first so we can discuss whether it aligns with the project's direction.

---

## Development Setup

### Prerequisites

- Latest stable [Rust](https://www.rust-lang.org/tools/install) toolchain
- [Node.js](https://nodejs.org/) (v18+)
- Windows 10/11 x64 **or** Linux x86_64 (Ubuntu 22.04+/Debian 12+/Fedora 40+)

On Linux, install the Tauri system dependencies first:

```bash
# Ubuntu / Debian
sudo apt-get install libwebkit2gtk-4.1-dev build-essential curl wget file \
  libssl-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev \
  patchelf libasound2-dev libdbus-1-dev

# Fedora
sudo dnf install webkit2gtk4.1-devel gtk3-devel libappindicator-gtk3-devel \
  librsvg2-devel patchelf alsa-lib-devel dbus-devel openssl-devel curl wget file
```

You also need a running Secret Service provider (GNOME Keyring or KWallet)
for API-key storage, and an X11 session for full voice-typing injection
(on Wayland, injection may fail - the text stays in the clipboard, paste it
with Ctrl+V).

### One-time sidecar setup (Windows and Linux)

`cargo check` and every build require the staged sidecar files in
`src-tauri/binaries/` (the Tauri bundler validates them before compiling).
Provision the prebuilt vendor bundle once per machine, then build + stage:

```bash
# Linux: fetch + verify the vendor bundle (digest pinned in
# src-tauri/moonshine-v2-server/VENDOR.linux.json)
mkdir -p .vendor/moonshine
curl -L "$(python3 -c "import json;print(json.load(open('src-tauri/moonshine-v2-server/VENDOR.linux.json'))['url'])")" \
  -o .vendor/moonshine/moonshine-voice-linux-x86_64.tar.gz
tar -xzf .vendor/moonshine/moonshine-voice-linux-x86_64.tar.gz -C .vendor/moonshine

# Build the sidecar (its build.rs stages the .so files into target/)
cargo build --manifest-path src-tauri/Cargo.toml -p moonshine-v2-server

# Mirror into the Tauri staging dir (same names CI uses)
TRIPLE=$(rustc --print host-tuple)
mkdir -p src-tauri/binaries
cp src-tauri/target/debug/moonshine-v2-server \
  "src-tauri/binaries/moonshine-v2-server-$TRIPLE"
cp src-tauri/target/debug/libonnxruntime.so.1 \
  src-tauri/binaries/libonnxruntime.so.1
cp src-tauri/target/debug/libmoonshine.so \
  src-tauri/binaries/libmoonshine.so
```

Windows contributors follow the same flow with `VENDOR.json` (see the
`fetch moonshine v2 vendor bundle` step in `.github/workflows/release.yml`).

### Getting Started

```bash
# Clone the repository
git clone https://github.com/raviumeshkulkarni-web/Fluence-Desktop.git
cd Fluence-Desktop

# Install dependencies
npm install

# Start development mode (Windows)
npm run dev

# Start development mode (Linux)
npm run dev:linux
```

### Useful Commands

| Command | Description |
|---|---|
| `npm run dev` | Start the app in development mode (Windows) |
| `npm run dev:linux` | Start the app in development mode (Linux) |
| `npm run build` | Build production installers (Windows: NSIS) |
| `npm run build:linux` | Build production bundles (Linux: deb + AppImage) |
| `npm run check` | Run Rust type checking |
| `npm run clippy` | Run Rust lints |
| `cargo fmt` | Format Rust code (run inside `src-tauri/`) |

For a Linux `.rpm` (Fedora), run the same build with `--bundles rpm` and
the same `--config` override as `build:linux` (see `package.json`).

---

## Project Structure

```
Fluence-Desktop/
├── src/                    # Frontend (HTML, JS, CSS)
│   ├── index.html         # Main settings window
│   ├── overlay.html       # Floating visualizer overlay
│   ├── wizard.html        # Onboarding wizard
│   ├── js/                # JavaScript modules
│   └── css/               # Stylesheets
├── src-tauri/             # Rust backend
│   └── src/
│       ├── audio.rs       # Audio capture & processing
│       ├── transcribe.rs  # API transcription logic
│       ├── workflow.rs    # Voice command workflows
│       ├── overlay.rs     # Overlay window management
│       ├── clipboard.rs   # Text injection
│       ├── settings.rs    # Settings persistence
│       └── tray.rs        # System tray management
└── docs/assets/           # README screenshots
```

---

---

## Coding Guidelines

### Rust (Backend)

- Follow [Rust 2021 edition](https://doc.rust-lang.org/edition-guide/rust-2021/) conventions
- Use 4-space indentation
- `snake_case` for functions and modules
- `PascalCase` for types and structs
- Keep Tauri command names stable (frontend depends on them)
- Handle errors explicitly with `Result<T, E>`

### JavaScript / HTML / CSS (Frontend)

- Use plain JavaScript (no frameworks)
- Use descriptive filenames: `audio-viz.js`, `design-tokens.css`
- Follow existing naming patterns (hyphenated for assets)

---

## Making Changes

1. **Fork** the repository
2. **Create a branch** for your change:
   ```bash
   git checkout -b feat/my-new-feature
   # or
   git checkout -b fix/my-bugfix
   ```
3. Make your changes
4. **Run checks** before committing:
   ```bash
   npm run check
   npm run clippy
   ```
5. **Format your code**:
   ```bash
   cd src-tauri && cargo fmt
   ```
6. **Manually test** your changes with `npm run dev`

---

## Before Opening a Pull Request

Please ensure that:

- The project builds successfully
- Existing functionality has not regressed
- The application has been tested manually for the affected workflow
- Your change is focused on a single concern
- Documentation is updated if behavior changed

---

## Pull Request Process

1. Push your branch to your fork
2. Open a Pull Request against `main`
3. In your PR description, include:
   - What changed and why
   - How to test the change
   - Screenshots or recordings (for UI changes)
4. Every Pull Request is reviewed before merging. Feedback is a normal part of the process and helps maintain the quality of the project.

> **Note:** Submitting a Pull Request does not guarantee that it will be merged. Maintainers may request revisions, suggest an alternative implementation, or decide not to merge a contribution if it does not align with the project's goals or quality standards.

### Commit Messages

Use clear, short prefixes:

- `feat:` - new feature
- `fix:` - bug fix
- `chore:` - maintenance task
- `docs:` - documentation update
- `release:` - version bump

Example: `fix: resolve overlay flickering on multi-monitor setup`

---

## Security

- **Never** commit API keys, credentials, or secrets
- Do not commit generated files in `src-tauri/target/`
- Do not commit downloaded model files
- API keys are stored securely via Windows Credential Manager - keep it that way

---

## Community

Please be respectful and constructive during discussions and code reviews. We value thoughtful technical discussions and welcome different perspectives.

---

## Questions?

If you have questions about contributing, feel free to open an issue with the label **"question"**.

---

## Good First Issues

If you're new to the project, look for issues labeled:

- `good first issue`
- `documentation`
- `help wanted`
