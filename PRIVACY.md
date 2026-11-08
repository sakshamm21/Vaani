# Privacy policy

Last updated: 8 November 2026

Vaani is a voice dictation app for Linux and Windows. It is written by Saksham Malhotra
and released as open source under the MIT licence. This policy covers every version of
Vaani, including the one in the Microsoft Store.

## What Vaani collects

Nothing. Vaani has no accounts, analytics, telemetry, advertising or crash reporting, and
it does not send any data to the developer.

## How your voice is handled

- Vaani only listens while you hold the dictation key, or between the start and stop
  commands.
- By default, speech is recognised on your own computer with an offline model. The audio
  stays in memory, is never written to disk and never leaves your computer.
- If you choose a cloud speech provider (for example Groq, OpenAI, Deepgram, ElevenLabs,
  AssemblyAI or Sarvam) and enter your own API key, your recordings are sent straight to
  that provider to be transcribed. The same applies to the optional AI clean-up, which
  sends the transcribed text to the provider you pick. That provider's own privacy policy
  then applies. Both features are off by default.

## What is stored on your computer

- Your settings.
- The speech models you download.
- A history of your recent dictations (the last 20 by default). In Settings you can choose
  how many are kept, keep none, or pause it.
- Logs, which are kept for seven days and contain no dictated text at the default log level.
- API keys you enter. These are kept in the system keyring (Windows Credential Manager on
  Windows).

None of this is uploaded anywhere. Uninstalling Vaani removes the app; you can delete
the files above at any time.

## Network requests

When you use a local model, the only network requests Vaani makes are model downloads from
the sherpa-onnx project's releases on GitHub. With a cloud provider, Vaani also connects
to that provider. If you turn on "Tell me when a new version is out" (off by default),
Vaani asks GitHub once a day for the latest Vaani release; the request carries only
Vaani's version number.

## Children

Vaani does not collect personal information from anyone, including children.

## Changes

Any change to this policy is published in this file, at
https://github.com/sakshamm21/Vaani/blob/main/PRIVACY.md.

## Contact

Questions: https://github.com/sakshamm21/Vaani/issues
