## Shared Fluence Components, UI/UX Consistency, and Accessibility Overhaul

### Highlights

- **Shared Component Library**: Introduced a standardized suite of accessible UI components (`PageHeader`, `SettingRow`, `ConnectionStatus`, `RouteErrorBanner`, `RouteErrorState`, `ChoiceRow`, `PromptEditorDialog`, `ApiKeyInput`, and `HotkeyRecorderField`) across all settings screens and the onboarding wizard.
- **Floating Bubble Studio Parity**: Full Agent Mode preview support in Settings > Floating Bubble. Users can toggle between Transcription mode (purple) and Agent mode (teal) to inspect live canvas waveforms and shadow DOM badges in real time.
- **Accessibility & WCAG Compliance**: Fixed nested interactive controls in History rows, added dedicated keyboard-accessible expand and collapse buttons with smooth chevron rotations, improved mode badge contrast, and added proper list semantics.
- **Honest Autosave Experience**: Replaced manual save buttons on General and Floating Bubble settings with clear auto-save indicators, while preserving the Ctrl+S shortcut for instant cache flushing.
- **Clustered Navigation**: Reorganized the sidebar into 4 intuitive clusters (Home, Capture, Intelligence, App) matching the core transcription workflow, paired with flex scroll containment that prevents navigation overlap on smaller viewports.
- **Design System Tokenization & Hygiene**: Replaced raw color literals with design tokens across settings and dialogs, cleaned up legacy onboarding CSS rules, and hardened API key mask toggling.

### What Stays the Same

Your dictation shortcuts, language models, history, snippets, custom agents, custom prompt styles, theme choices, and sync data remain fully intact.

### Known Limitations

- The installer is not code-signed, so Windows SmartScreen may warn the first time you run it. Choose "More info" > "Run anyway". This is unchanged from previous releases.
- Agent mode carries out its actions without asking first, so a misheard command can act on the active window.
- Text pastes wherever your cursor is focused when transcription finishes. Switching apps while dictating pastes into whichever window is active.
