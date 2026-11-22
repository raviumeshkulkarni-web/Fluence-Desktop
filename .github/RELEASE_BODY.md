## 5-step onboarding, in-wizard offline models, and backend security hardening

### Streamlined 5-step onboarding

Onboarding is now fast and frictionless, trimmed from 9 screens down to 5:
1. **Welcome**: Clean and instant start.
2. **Choose Your Voice Engine**: Pick between Cloud API (Groq, OpenAI, Mistral, Custom) or Local Offline. When offline is selected, choose between Moonshine v2 Small (~142 MB, fast English) and SenseVoice (~263 MB, multilingual) with 1-click download, live progress, and cancellation.
3. **Set Your Hotkey**: Configure your dictation shortcut and recording mode.
4. **Test Your Setup**: Live microphone testing with the Record orb for both cloud API and installed local offline models (verifying the real sidecar and audio capture end-to-end).
5. **You're All Set!**: Direct launch into voice typing with instant tray minimization or optional Settings navigation.

Theme, overlay position, and style configuration are cleanly deferred to Settings.

### Local offline transcription enhancements

- **Faster sidecar crash diagnostics**: Local ASR startup now drains stderr continuously and fails fast with clear status labels if a runtime DLL or sidecar fails to launch.
- **In-wizard downloads**: Offline speech models can be downloaded and tested directly during initial setup without running into missing-model errors.
- **Graceful sidecar termination**: The local ASR sidecar process is cleanly terminated on app exit and auto-update relaunch, preventing orphaned processes, high idle memory usage, and locked binary files.
- **Safe download state machine**: Model downloads use atomic guards with RAII drop protection, throttled IPC progress emits to prevent UI stutter, and automatic cleanup of partial temp files on failure or cancellation.
- **Honest testing**: If offline model download is skipped, the test step honestly guides you to Settings > Providers instead of reporting a false ready state.

### Security and privacy hardening

- **Credential safety**: Protected credential blob parsing against null pointers and zero-length buffers in Windows Credential Manager, preventing unexpected crashes on empty credential entries.
- **Strict URL validation**: Hardened production transcription and AI polish HTTP senders with URL validation to reject cleartext HTTP transmissions on non-loopback endpoints.
- **Privacy leak prevention**: AI polish now prevents sending unauthenticated transcripts over the network when no API key is configured.
- **LLM completion resilience**: Elevated chat completion limits to 4,096 tokens with fail-closed handling for length truncation and nullable provider responses.
- **Auto-learn categorization**: Per-app AI cleanup overrides are correctly categorized as rewrites to keep dictionary suggestions clean.
- **ACL updates**: Expanded command access controls to allow the setup wizard to query and download offline models and run setup mic tests while keeping model deletion restricted to the main window.

### What stays the same

Your dictation shortcuts, language models, history, snippets, custom agents, custom prompt styles, theme choices, and sync data remain fully intact.

### Known limitations

- The installer is **not code-signed**, so Windows SmartScreen may warn the first time you run it. Choose "More info" > "Run anyway". This is unchanged from previous releases.
- Agent mode carries out its actions without asking first, so a misheard command can act on the window you are in.
- Text goes wherever your cursor is when transcription finishes; switching apps while dictating pastes into whichever app you end up in.
- Only the first portion of vocabulary terms fits in the speech-model prompt; the Dictionary page reports this limit. All saved corrections still apply after transcription.
