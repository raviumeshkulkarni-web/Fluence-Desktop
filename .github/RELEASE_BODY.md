## Spoken punctuation, dictionary packs, history export, and snippet placeholders

### Spoken punctuation

You can now format dictation by speaking punctuation and line-break commands. Turn on **General > Dictation > Spoken Punctuation** and say "comma", "period", "question mark", "exclamation mark", "colon", "semicolon", "new line", or "new paragraph".

- Works with online and offline transcription.
- Runs before dictionary corrections and snippet expansion.
- Normalizes punctuation already added by the speech model, so a spoken comma surrounded by commas no longer becomes `,,,`.
- Off by default because these command words also occur in normal speech.

### Dictionary term packs

The Dictionary page now has an **Add Pack** button with curated correction sets for development, AI and data, business, healthcare, and legal terms. Entries already in your dictionary are skipped, and partial failures are reported without hiding the entries that were saved.

### History and dictionary export

- History can be exported as TXT, CSV, or JSON from the History page.
- Exports include every transcription on the device, independent of search and filters.
- CSV exports quote cells and neutralize spreadsheet formula injection.
- History and dictionary exports use the native Windows save dialog instead of relying on a WebView download.

### Snippet placeholders

Snippet expansion text can now use `{date}`, `{time}`, `{datetime}`, `{day}`, and `{year}`. Placeholders expand locally when the snippet is inserted; unknown placeholders and plain transcript text are left unchanged.

### Reliability improvements

- Auto-learn no longer treats punctuation changes made by spoken formatting as user corrections.
- Spoken formatting is device-local and does not sync, so enabling it on one device does not change another device's dictation behavior.
- Added regression coverage for spoken formatting, term-pack imports, history export, snippet placeholders, and native save behavior.

### What stays the same

Your dictation shortcuts, language models, history, snippets, custom agents, custom prompt styles, theme choices, and sync data remain fully intact.

### Known limitations

- The installer is **not code-signed**, so Windows SmartScreen may warn the first time you run it. Choose "More info" > "Run anyway". This is unchanged from previous releases.
- Agent mode carries out its actions without asking first, so a misheard command can act on the window you are in.
- Text goes wherever your cursor is when transcription finishes; switching apps while dictating pastes into whichever app you end up in.
- Spoken formatting is opt-in and deterministic: ordinary uses of words such as "period" or "comma" are converted while it is enabled. It collapses repeated copies of the same punctuation, but a different mark supplied by the speech model immediately next to a spoken command may still appear.
- Only the first portion of vocabulary terms fits in the speech-model prompt; the Dictionary page reports this limit. All saved corrections still apply after transcription.
