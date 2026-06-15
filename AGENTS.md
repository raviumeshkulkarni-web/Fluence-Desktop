# Repository Guidelines

## Project Structure & Module Organization

Fluence is a Windows-focused Tauri v2 desktop app. Frontend HTML lives in `src/` (`index.html`, `overlay.html`, `wizard.html`), with view-specific JavaScript in `src/js/` and styles in `src/css/`. Rust backend code is in `src-tauri/src/`; important modules include `audio.rs`, `transcribe.rs`, `workflow.rs`, `overlay.rs`, `clipboard.rs`, `settings.rs`, and `tray.rs`. Tauri configuration and permissions are in `src-tauri/tauri.conf.json` and `src-tauri/capabilities/`. App icons are under `src-tauri/icons/`, while README screenshots are in `docs/assets/`.

## Build, Test, and Development Commands

- `npm install`: install Node/Tauri CLI dependencies from `package-lock.json`.
- `npm run dev`: start the Tauri development app.
- `npm run build`: build production Windows bundles under `src-tauri/target/release/bundle/`.
- `npm run check`: run `cargo check` in `src-tauri`.
- `npm run clippy`: run Rust lints with Cargo Clippy.
- `cd src-tauri && cargo fmt`: format Rust sources before submitting backend changes.

## Coding Style & Naming Conventions

Use Rust 2021 conventions: four-space indentation, `snake_case` functions/modules, `PascalCase` types, and small modules with explicit error handling. Keep Tauri command names stable because frontend code invokes them across the JS/Rust boundary. Frontend files use plain HTML, CSS, and JavaScript; keep naming consistent with existing hyphenated asset files (`audio-viz.js`, `design-tokens.css`) and view-specific scripts (`settings.js`, `wizard.js`, `overlay.js`).

## Testing Guidelines

There is no dedicated test harness in the current tree. For every change, run `npm run check` at minimum; run `npm run clippy` for Rust behavior changes. Manually verify affected desktop flows with `npm run dev`, especially global hotkeys, recording start/stop, transcription, text injection, settings persistence, overlay window controls, and offline model management.

## Commit & Pull Request Guidelines

Recent history uses short release/chore/feature-style subjects, for example `release: v1.1.8 - Fix latency...`, `chore: bump version to v1.1.5`, and `feat: upgrade background...`. Keep commits focused and imperative. Pull requests should describe the user-visible change, list validation commands run, note any untested Windows flows, and include screenshots or short recordings for UI changes.

## Security & Configuration Tips

Do not commit API keys, local model files, generated bundles, or `src-tauri/target/`. Preserve Windows Credential Manager storage for secrets, and review `src-tauri/capabilities/default.json` whenever adding new frontend-accessible commands.
