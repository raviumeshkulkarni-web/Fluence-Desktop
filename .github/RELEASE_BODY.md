<!--
     Release notes convention: this file ALWAYS contains the notes the next
     pushed tag publishes. release.yml reads it verbatim, so there is exactly
     one copy, edit here, never in the workflow. Replace this ENTIRE file
     (this comment included) when cutting a release. Keep the language plain:
     write for the person using Fluence, not for the person building it. -->

## Waveform animation choices and cleaner completion

### Waves or Amp Bars for your recording overlay

You can now customize how the floating recording bubble animates while you speak. In Settings under Floating Bubble, look for the new **Waveform Animation** section:
- **Waves**: Three layered ripples. The signature animation Fluence ships with.
- **Amp Bars**: Vertical equalizer bars that rise and fall with your voice volume.

The selected style shows a live preview directly in the settings window, and adapts to your system motion settings.

### Cleaner completion feedback

When Fluence finishes typing your dictation into an app, the overlay now gives you a clean, distraction-free visual checkmark without obscuring your screen with extra text. The text remains fully accessible for screen readers and tooltips.

### What stays the same

Your audio detection, dictation shortcuts, language models, history, snippets, and sync data are completely unaffected. The waveform animation pick is saved locally on your device.

### Known limitations

- The installer is **not code-signed**, so Windows SmartScreen may warn the first time you run it. Choose "More info" → "Run anyway". This is unchanged from previous releases.
- Agent mode carries out its actions without asking first, so a misheard command can act on the window you are in.
- Text goes wherever your cursor is when transcription finishes; switching apps while dictating pastes into whichever app you end up in.
- An image or file list on your clipboard is replaced by a paste and cannot be restored afterwards. You are told when this happens.
