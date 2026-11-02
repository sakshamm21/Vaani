# Vaani

**Speak, don't type.** Vaani is open-source voice dictation for Linux. Hold a key, speak, let
go, and your words appear in whatever app you're typing in: browser, editor, chat app or
terminal.

It runs a speech model on your own computer by default, so your voice never leaves it. If you
prefer, you can use your own cloud API key instead. No account, no server, no telemetry.

## Features

- **Works in every app.** Text is pasted into the focused window, and your clipboard is put
  back afterwards.
- **Hold to talk, or tap for hands-free.** Hold Right Ctrl while you speak. Tap it once to keep
  recording, and tap again to finish. Esc cancels.
- **Private by default.** Local models via [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx);
  12 to choose from, downloaded from inside the app.
- **Cloud if you want it.** Groq, OpenAI or any OpenAI-compatible server (for example a local
  Whisper server). Keys are stored in your system keyring, never in a file. A cloud icon shows
  on the pill whenever audio leaves your machine.
- **Floating pill** with a live level meter while you speak, and a tray icon for quick actions.
- **Settings window** with a try-it box, shortcut picker, microphone choice, model manager,
  cloud setup and a searchable history of everything you've dictated.
- **Light and dark themes** in soft rose, switchable inside the app.
- **Light on your machine.** Native GTK4, no browser engine. Idle in the tray it uses about 0%
  CPU and roughly 40 MB of its own memory.
- **Clean text.** Filler words ("um", "uh") removed, sentences capitalised, a space added
  between dictations.

## Requirements

- Linux on **X11**: Linux Mint (Cinnamon, MATE, Xfce), Ubuntu on Xorg, Debian, Pop!_OS and
  similar. Wayland is not supported yet.
- A microphone.
- About 1 GB of free memory for the recommended model (smaller models need less).

## Install

```sh
git clone https://github.com/sakshamm21/Vaani.git
cd Vaani
./scripts/install.sh
```

The script:

1. installs the build packages it needs with `apt` (asks for your password once, only if
   something is missing),
2. installs Rust with rustup if you don't have it,
3. builds Vaani (a few minutes the first time),
4. puts `vaani` and `vaani-worker` in `~/.local/bin`, adds Vaani to your app menu, and starts it.

On first start Vaani opens its window and offers to download the recommended speech model
(Parakeet TDT 0.6B v3, about 465 MB). After that it starts automatically when you log in;
turn that off under General.

If `vaani` isn't found in a terminal right after installing, log out and back in so
`~/.local/bin` is on your PATH. The app menu entry works straight away.

To remove it: `./scripts/install.sh --uninstall`. Your settings, history and models stay in
`~/.config/vaani` and `~/.local/share/vaani` until you delete them.

## Using it

| Do this | To |
| --- | --- |
| Hold **Right Ctrl**, speak, let go | Dictate into the focused app |
| Tap **Right Ctrl**, speak, tap again | Dictate hands-free |
| **Esc** while recording | Cancel |
| Tray icon → Paste last | Paste the last dictation again |
| App menu → Vaani | Open settings |

You can change the key (Right Alt, a function key and others) and the mode under
**General**. Switch light or dark with the sun/moon button at the top of the window.

### Speech models

| Model | Size | Languages |
| --- | --- | --- |
| Parakeet TDT 0.6B v3 (recommended) | 465 MB | English and 24 European languages |
| Parakeet TDT 0.6B v2 | 460 MB | English |
| Moonshine Tiny / Base | 103 / 240 MB | English, fastest on slow machines |
| Whisper Tiny / Base / Small | 111 / 198 / 610 MB | 99 languages, including Hindi |
| Whisper Large v3 Turbo | 538 MB | 99 languages |
| Distil-Whisper Large v3.5 | 504 MB | English |
| Whisper Medium / Large v3 | 1842 / 1019 MB | 99 languages |
| SenseVoice Small | 155 MB | Chinese, English, Japanese, Korean, Cantonese |

Download, switch and delete models on the **Speech model** page, or from a terminal with
`vaani models`.

### Commands

| Command | What it does |
| --- | --- |
| `vaani` / `vaani run` | Start the app (only one copy runs at a time) |
| `vaani settings` | Open the window (starts the app if needed) |
| `vaani toggle` / `start` / `stop` / `cancel` | Control dictation, e.g. from a custom shortcut |
| `vaani status` | Print `idle`, `recording` or `processing` |
| `vaani paste-last` | Paste the last dictation again |
| `vaani models [list \| download ID --use \| remove ID]` | Manage speech models |
| `vaani history [N]` | Print recent dictations |
| `vaani doctor [--json]` | Report your setup, for bug reports |
| `vaani quit` | Stop the app |

### Files

- Settings: `~/.config/vaani/config.toml` (plain text, applied as soon as you save it)
- Models and history: `~/.local/share/vaani/`
- Logs: `~/.local/state/vaani/logs/` (set `VAANI_LOG=debug` for more detail)

## Known issues

- **Wayland** sessions are not supported yet (global keys and pasting need a different approach
  there). Use an X11/Xorg session for now.
- **Some editors that cache the clipboard** (for example Nextpad++, a GTK port of Notepad++)
  could miss the paste. A fix that waits 40 ms before pasting is in this version but hasn't
  been confirmed in Nextpad++ yet. If text doesn't appear, it's still on your clipboard and in
  History.
- `vaani doctor` may print harmless JACK/ALSA messages while it lists audio devices.

## Roadmap

- Confirm paste in more apps (Nextpad++, Wine apps, LibreOffice, VS Code).
- Wayland: GNOME, KDE, Hyprland, Sway.
- Optional AI clean-up (punctuation, formatting) with local or cloud LLMs.
- More cloud providers and models.
- Packages: .deb, Flatpak, AppImage.

## Building from source

You need Rust 1.85+ and, on Debian/Ubuntu/Mint:
`build-essential pkg-config clang cmake libasound2-dev libgtk-4-dev libx11-dev libxi-dev libxtst-dev libxcb1-dev`.

```sh
cargo build --release
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

### Layout

```
crates/
  vaani-app/       the `vaani` binary: background app, tray, pill, settings window, D-Bus, CLI
  vaani-worker/    speech-to-text process (sherpa-onnx), separate so a crash can't take the app down
  vaani-audio/     microphone capture and resampling
  vaani-stt/       local and cloud speech engines
  vaani-hotkey/    global key (X11 XInput2)
  vaani-inject/    paste into the focused app (clipboard + XTest)
  vaani-text/      clean-up (fillers, capitals, spacing)
  vaani-store/     history (SQLite) and API keys (system keyring)
  vaani-models/    model catalogue and downloads
  vaani-core/      state machine, config, paths
  vaani-platform/  session detection
models/            model catalogue (registry.toml)
assets/            icon, app-menu entry, fonts
scripts/           installer
docs/              platform notes
```

## Licence

Vaani is dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
The bundled fonts, Unbounded and Plus Jakarta Sans, are under the SIL Open Font License (see
`assets/fonts/`).
