## Agent overlay picker, error sanitization, and setup wizard hardening

### Agent overlay picker with pre-staged geometry

You can now switch active agents directly from the floating overlay. The agent picker card expands and docks smoothly without window reflow or geometry jitter, matching your active overlay tier and screen placement.

### Error sanitization and privacy hardening

Diagnostic dumps, file paths, model filenames, thread panics, and internal operating system error codes no longer leak into user-facing notifications or dialogs. Error states across the Dashboard, History, and Providers views now provide clear, actionable descriptions and in-place retry options.

### First-run wizard data preservation

Completing or re-running setup now reads existing configuration first and safely merges your changes, preventing custom sync, cleaner provider, audio ducking, and waveform settings from being reset to defaults. Closing the wizard with unfinished setup now prompts for confirmation instead of exiting silently.

### Overlay status indicators and recovery layout

The overlay now cleanly reports `DONE` upon completion and `ERROR` on failure rather than remaining stuck on `LISTENING`. In the compact bubble tier, an accessible circular retry affordance is available on failure, and full-tier recovery actions (Copy and Retry) are neatly aligned side by side.

### What stays the same

Your dictation shortcuts, language models, history, snippets, custom agents, custom prompt styles, theme choices, and sync data remain fully intact.

### Known limitations

- The installer is **not code-signed**, so Windows SmartScreen may warn the first time you run it. Choose "More info" → "Run anyway". This is unchanged from previous releases.
- Agent mode carries out its actions without asking first, so a misheard command can act on the window you are in.
- Text goes wherever your cursor is when transcription finishes; switching apps while dictating pastes into whichever app you end up in.
- With the clipboard toggle on, the app no longer brings back what you had copied before dictating, so your earlier copied content is not recoverable after a dictation. Turn the toggle off to restore that behavior.
- An image or file list on your clipboard is replaced by a paste and cannot be restored afterwards. You are told when this happens.
- The window has no drop shadow, and scrolling content can briefly slide under the floating title bar during route changes, as in any modern window with an integrated title bar.
