## Two rough edges from 1.33.0, smoothed

### History day headers sit flush again

Scrolling the History list no longer opens a gap between the pinned day heading ("Today", "Yesterday", and so on) and the search and filter bar above it. The heading stays attached directly below the chrome, with rows passing cleanly underneath it instead of showing through the empty band above it.

### Clean window corners

The window's rounded corners no longer carry a faint dark hairline tracing their edge. When the native window shadow was disabled in 1.33.0, a leftover shadow inside the window kept bleeding through the transparent corner cutouts. It is now removed, so the corners blend cleanly into whatever is behind them in both dark and light themes.

### What stays the same

Everything else from 1.33.0 is unchanged: the unified window with its integrated title bar, the clipboard auto-copy toggle in General settings, and your audio detection, shortcuts, language models, history, snippets, agents, styles, settings, and sync data. The onboarding wizard, the floating overlay, and all commands behave exactly as before.

### Known limitations

- The installer is **not code-signed**, so Windows SmartScreen may warn the first time you run it. Choose "More info" → "Run anyway". This is unchanged from previous releases.
- Agent mode carries out its actions without asking first, so a misheard command can act on the window you are in.
- Text goes wherever your cursor is when transcription finishes; switching apps while dictating pastes into whichever app you end up in.
- With the clipboard toggle on, the app no longer brings back what you had copied before dictating, so your earlier copied content is not recoverable after a dictation. Turn the toggle off to restore that behavior.
- An image or file list on your clipboard is replaced by a paste and cannot be restored afterwards. You are told when this happens.
- The window has no drop shadow, and scrolling content can briefly slide under the floating title bar during route changes, as in any modern window with an integrated title bar.
