## Brand Logo Invariance Across Themes & Presets

### Immutable Fluence Wordmark & Brand Colors

The Fluence brand wordmark and logo marks now remain constant across all UI themes, light/dark modes, and accent theme presets:

- **Canonical Brand Cyan Lock**: The `"ence"` letters in the Fluence wordmark are now strictly locked to the brand's canonical cyan (`#0BD6E3`) across all surfaces, including the Sidebar header, First-Run Wizard, and About view.
- **Accent Theme Separation**: Accent theme presets (Fluence Original, Orange, Olive Green, Yellow, Rose, Monochrome) and appearance modes (Dark, Light, System) now exclusively skin functional accent roles (interactive buttons, switches, selection cards, focus rings) without altering the brand logo wordmark.
- **Token Invariance**: Added `--color-logo-cyan` as an immutable brand token in `:root` and removed light-mode overrides that previously altered the logo's appearance.
- **Automated Regression Protection**: Added `tests/logo-theme-invariance.test.mjs` verifying that the brand logo and wordmark styling cannot regress in future builds.

### What stays the same

Your dictation shortcuts, language models, history, snippets, custom agents, custom prompt styles, theme choices, and sync data remain fully intact.

### Known limitations

- The installer is **not code-signed**, so Windows SmartScreen may warn the first time you run it. Choose "More info" → "Run anyway". This is unchanged from previous releases.
- Agent mode carries out its actions without asking first, so a misheard command can act on the window you are in.
- Text goes wherever your cursor is when transcription finishes; switching apps while dictating pastes into whichever app you end up in.
