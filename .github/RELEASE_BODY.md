## Update notes review dialog, STT retry classification, and Windows hotkey enhancements

### Release notes review dialog

You can now review complete release notes before downloading and staging an update. Clicking "Review update" in the sidebar or About view opens a clean modal dialog with structured highlights and changes instead of crowded inline text.

### Smart STT error classification and retry affordances

Transcription failures now cleanly distinguish between transient network/server issues and permanent configuration or authentication errors. Retrying is only offered when re-sending audio can succeed, preventing dead retry loops on missing API keys or invalid URLs. In compact and bubble overlay styles, the retry target is larger, fully keyboard-accessible, and easier to activate.

### Windows hotkey normalization and input protection

Shortcuts now natively support Numpad Plus (`NumPlus`), Windows key combinations (`Win`), and equal/plus key bindings. The hotkey recorder now validates input to prevent accidental bare-key bindings that could capture normal typing.

### Flicker-free view transitions and animation optimization

Page navigations on frameless transparent windows now transition smoothly without desktop background flicker. Outgoing and incoming views use shared-axis directional motion and respect the system reduced-motion preference. Chart rendering performance has been improved by eliminating redundant animations on re-renders.

### Whitespace trimming on speech-to-text output

Whisper prompt-seeded leading and trailing whitespace is now cleaned at the provider boundary, ensuring pasted dictations never insert unwanted spaces.

### What stays the same

Your dictation shortcuts, language models, history, snippets, custom agents, custom prompt styles, theme choices, and sync data remain fully intact.

### Known limitations

- The installer is **not code-signed**, so Windows SmartScreen may warn the first time you run it. Choose "More info" → "Run anyway". This is unchanged from previous releases.
- Agent mode carries out its actions without asking first, so a misheard command can act on the window you are in.
- Text goes wherever your cursor is when transcription finishes; switching apps while dictating pastes into whichever app you end up in.
- With the clipboard toggle on, the app no longer brings back what you had copied before dictating, so your earlier copied content is not recoverable after a dictation. Turn the toggle off to restore that behavior.
- An image or file list on your clipboard is replaced by a paste and cannot be restored afterwards. You are told when this happens.
- The window has no drop shadow, and scrolling content can briefly slide under the floating title bar during route changes, as in any modern window with an integrated title bar.
