## High-definition brand icon suite and crystal-clear Windows logo rendering

### Crystal-clear brand icons across Windows

The Windows desktop application now uses the canonical high-definition Fluence icon suite synchronized with the Fluence brand kit and Fluence Capture Desktop:

- **Windows Taskbar & Alt-Tab**: Deployed dedicated multi-frame `.ico` containing crisp, tailored assets for all Windows display scale factors (16×16, 20×20, 24×24, 32×32, 40×40, 48×48, 64×64, 128×128, 256×256), eliminating blurriness on standard and high-DPI displays.
- **System Tray**: Now displays the high-contrast, transparent system tray mark with sharp edges across dark and light taskbars.
- **In-App Branding**: Rendered with exact vector geometry and `shapeRendering="geometricPrecision"` across the Sidebar, About view, and First-Run Wizard, using the official brand typography and canonical colors.

### Build pipeline resource synchronization

Added `rerun-if-changed` triggers to the Rust build pipeline (`build.rs`) ensuring icon resource updates are always re-compiled into `resource.lib` without stale binary cache re-linking.

### What stays the same

Your dictation shortcuts, language models, history, snippets, custom agents, custom prompt styles, theme choices, and sync data remain fully intact.

### Known limitations

- The installer is **not code-signed**, so Windows SmartScreen may warn the first time you run it. Choose "More info" → "Run anyway". This is unchanged from previous releases.
- Agent mode carries out its actions without asking first, so a misheard command can act on the window you are in.
- Text goes wherever your cursor is when transcription finishes; switching apps while dictating pastes into whichever app you end up in.
