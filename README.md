# Vaani

Vaani is a voice dictation app for Linux. Hold a key, speak, and release: the text is typed
into whichever application has focus. Speech recognition runs locally by default, so audio
never leaves your computer. A cloud provider can be used instead if you supply your own API key.

- Works in any application that accepts pasted text: browsers, editors, office suites, chat
  clients and terminals.
- Local speech recognition with [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx), with a
  choice of twelve models (Parakeet, Whisper, Moonshine, SenseVoice).
- Optional cloud transcription through Groq, OpenAI or any OpenAI-compatible endpoint.
- Hold-to-talk or hands-free mode, a configurable shortcut, and Esc to cancel.
- A small on-screen indicator while recording, a tray icon, and a settings window with
  history, model management and light and dark themes.
- Native GTK 4 application with no browser engine. When idle it uses no measurable CPU.

## Contents

1. [Requirements](#requirements)
2. [Installation](#installation)
3. [First run](#first-run)
4. [Using Vaani](#using-vaani)
5. [Settings](#settings)
6. [Speech models](#speech-models)
7. [Cloud providers](#cloud-providers)
8. [Command line](#command-line)
9. [Files and privacy](#files-and-privacy)
10. [Troubleshooting](#troubleshooting)
11. [Updating and uninstalling](#updating-and-uninstalling)
12. [Building from source](#building-from-source)
13. [Project layout](#project-layout)
14. [Roadmap](#roadmap)
15. [License](#license)

## Requirements

| | |
| --- | --- |
| Display server | X11 (Xorg). Wayland is not supported yet. |
| Distributions | Linux Mint 21 or later, Ubuntu 22.04 or later, Debian 12, Pop!_OS and other Debian-based systems. Other distributions work if the build dependencies are installed by hand. |
| Desktop | Any X11 desktop. Tested on Cinnamon; MATE, Xfce, KDE Plasma (X11) and GNOME on Xorg are expected to work. |
| Memory | About 1 GB free for the default model. Smaller models need 200 to 400 MB. |
| Disk | About 1 GB for the build, plus 100 MB to 2 GB per model. |
| Other | A microphone, and an internet connection for the first model download. |

To check your session type, run `echo $XDG_SESSION_TYPE`. It should print `x11`.

## Installation

```sh
git clone https://github.com/sakshamm21/Vaani.git
cd Vaani
./scripts/install.sh
```

The install script:

1. Installs the build dependencies with `apt` if any are missing. This is the only step that
   asks for your password.
2. Installs the Rust toolchain through [rustup](https://rustup.rs) if `cargo` is not found.
   It is installed for your user only.
3. Builds Vaani in release mode. The first build takes three to ten minutes.
4. Installs `vaani` and `vaani-worker` to `~/.local/bin`, the application icon and menu entry
   to `~/.local/share`, and the interface fonts to `~/.local/share/fonts/vaani`.
5. Starts Vaani.

Nothing is installed system-wide apart from the build dependencies.

If `~/.local/bin` is not on your `PATH`, the script prints a notice. Most distributions add it
automatically at your next login. Until then, Vaani can be started from the application menu.

### Manual dependency installation

On Debian, Ubuntu and Mint the script installs these packages for you:

```sh
sudo apt install build-essential pkg-config clang cmake libasound2-dev libgtk-4-dev \
  libx11-dev libxi-dev libxtst-dev libxcb1-dev
```

On Fedora (untested):

```sh
sudo dnf install gcc gcc-c++ pkgconf clang cmake alsa-lib-devel gtk4-devel \
  libX11-devel libXi-devel libXtst-devel libxcb-devel
```

## First run

When Vaani starts for the first time it opens the settings window on the **Overview** page.

1. **Download a speech model.** Click **Download** in the status panel to fetch the default
   model, Parakeet TDT 0.6B v3 (465 MB). Progress is shown in the window. When the download
   finishes, the model loads automatically. This takes a few seconds.
2. **Try it.** Click in the test area on the Overview page, hold **Right Ctrl**, say a
   sentence, and release the key.
3. **Close the window.** Vaani keeps running in the background and appears in the system tray.

Vaani adds itself to your desktop's startup applications, so it starts at every login. You
can turn this off under **General → Start Vaani when I log in**.

No additional permissions are required on X11. Vaani does not need to be in the `input` group
and does not use `sudo` at runtime.

## Using Vaani

| Action | Result |
| --- | --- |
| Hold **Right Ctrl**, speak, release | Records while the key is held, then inserts the text. |
| Tap **Right Ctrl**, speak, tap again | Hands-free: recording continues until the second tap. |
| **Esc** while recording | Cancels. Nothing is inserted. |
| Tray icon → **Paste last** | Inserts the most recent dictation again. |
| Tray icon → **Settings**, or Vaani in the application menu | Opens the settings window. |

While recording, an indicator at the bottom of the screen shows the input level and elapsed
time. If a cloud engine is active, the indicator says so.

Text is inserted by placing it on the clipboard and sending a paste shortcut to the focused
window (Ctrl+V, or Ctrl+Shift+V in terminals). Your previous clipboard contents are restored
afterwards. Each dictation is also saved to History before it is inserted, so text is not lost if an
application does not accept the paste. History keeps the last 20 dictations by default; this
can be changed, or turned off, under **General → History**.

Recordings are limited to five minutes.

## Settings

Open the settings window from the tray icon or the application menu. Changes are saved
immediately.

**General**

| Setting | Description |
| --- | --- |
| Theme | Light, dark, or follow the desktop. Also available from the button in the title bar. |
| Dictation key | Click **Change…** and press the key to use. Supported keys: Right Ctrl, Right Alt, Right Shift, Right Super, Left Ctrl, Left Alt, Super, Menu, Insert, Pause, Scroll Lock and F1 to F12. |
| How the key works | *Hold to talk* (tap for hands-free) or *Press to start, press to stop*. |
| Input device | The microphone to record from. |
| Sounds | Short tones when recording starts and stops. |
| Floating pill | The on-screen recording indicator. |
| Remove filler words | Removes "um", "uh" and similar words. |
| Add a space after each dictation | Separates consecutive dictations. |
| Start Vaani when I log in | Adds or removes the autostart entry. |
| History | How many dictations to keep: none, the last 20 (default), 100 or 1,000, or all. Older entries are deleted automatically. |

**Speech model** chooses between local and cloud recognition, sets the language, and manages
downloaded models.

**Cloud** configures the provider, API key, model and endpoint for cloud recognition.

**History** lists previous dictations with search and copy. **Clear all** deletes them.

**About** shows the version and links to the log, model and configuration folders, and copies
a diagnostic report for bug reports.

All settings are stored in `~/.config/vaani/config.toml`. The file can be edited by hand;
Vaani applies changes when the file is saved.

## Speech models

| Model | Download | Languages | Notes |
| --- | --- | --- | --- |
| Parakeet TDT 0.6B v3 | 465 MB | English and 24 European languages | Default. Best accuracy and speed on most CPUs. |
| Parakeet TDT 0.6B v2 | 460 MB | English | |
| Moonshine Tiny | 103 MB | English | Fastest. For older or low-memory machines. |
| Moonshine Base | 240 MB | English | |
| Whisper Tiny | 111 MB | 99 languages | Includes Hindi. |
| Whisper Base | 198 MB | 99 languages | |
| Whisper Small | 610 MB | 99 languages | |
| Whisper Large v3 Turbo | 538 MB | 99 languages | |
| Distil-Whisper Large v3.5 | 504 MB | English | |
| Whisper Medium | 1842 MB | 99 languages | Slow on CPU. |
| Whisper Large v3 | 1019 MB | 99 languages | Slow on CPU. |
| SenseVoice Small | 155 MB | Chinese, English, Japanese, Korean, Cantonese | |

Models are managed on the **Speech model** page, where you can download, select and delete
them. They can also be managed with `vaani models`. Models are stored in
`~/.local/share/vaani/models`.

The active model stays loaded in a separate `vaani-worker` process so that transcription
starts immediately. The worker uses about 800 MB with Parakeet, and much less with the
smaller models. If the worker crashes, Vaani restarts it.

For Whisper and the cloud engines, set the spoken language under **Speech model → Language**,
or leave it on automatic detection. Parakeet detects its supported languages automatically.

## Cloud providers

| Provider | Default model | API key |
| --- | --- | --- |
| Groq | `whisper-large-v3-turbo` | [console.groq.com/keys](https://console.groq.com/keys) (free tier available) |
| OpenAI | `gpt-4o-mini-transcribe` | [platform.openai.com/api-keys](https://platform.openai.com/api-keys) |
| OpenAI-compatible | set by you | Any server implementing `/v1/audio/transcriptions`, for example a local faster-whisper or whisper.cpp server |

To use a cloud provider:

1. Open **Cloud**, choose a provider, paste your API key and click **Save key**.
2. Click **Use cloud engine**, or select **Cloud API** on the **Speech model** page.

API keys are stored in the system keyring (GNOME Keyring or KWallet, through the Secret
Service API). They are never written to the configuration file. Audio is sent to the provider
only while a cloud engine is selected.

## Command line

| Command | Description |
| --- | --- |
| `vaani` or `vaani run` | Start Vaani. Only one instance runs at a time. |
| `vaani settings` | Open the settings window, starting Vaani if needed. |
| `vaani toggle` | Start or finish a dictation. |
| `vaani start`, `vaani stop`, `vaani cancel` | Control a dictation. |
| `vaani status` | Print the current state and engine. |
| `vaani paste-last` | Insert the most recent dictation again. |
| `vaani models` | List models and whether they are installed. |
| `vaani models download <id> [--use]` | Download a model, and optionally make it active. |
| `vaani models remove <id>` | Delete a downloaded model. |
| `vaani history [count]` | Print recent dictations. |
| `vaani doctor [--json]` | Print a diagnostic report. |
| `vaani quit` | Stop Vaani. |

`vaani toggle` can be bound to a key in your desktop's keyboard settings if you prefer a key
combination that Vaani's own shortcut does not support.

## Files and privacy

| Path | Contents |
| --- | --- |
| `~/.config/vaani/config.toml` | Settings |
| `~/.local/share/vaani/models/` | Downloaded models |
| `~/.local/share/vaani/history.db` | Dictation history (SQLite). Keeps the last 20 by default; see **General → History**. |
| `~/.local/state/vaani/logs/` | Logs, kept for seven days |
| `~/.config/autostart/io.github.sakshamm21.Vaani.desktop` | Autostart entry |

Vaani has no accounts, analytics or telemetry. With a local model it makes network requests
only to download models from the
[sherpa-onnx releases](https://github.com/k2-fsa/sherpa-onnx/releases) on GitHub. Audio
recordings are kept in memory only and are never written to disk.

## Troubleshooting

Start with `vaani doctor`. It reports the session type, desktop, microphone, hotkey, model
and text-insertion status. Include its output in bug reports.

**Nothing happens when I press the key.**
Check that Vaani is running (`vaani status`) and that you are on an X11 session
(`echo $XDG_SESSION_TYPE`). Another application may be using the same key; choose a different
key under **General → Dictation key**.

**The text is not inserted into one application.**
The text is still on the clipboard and in History, so you can paste it with Ctrl+V. Please
open an issue naming the application. Applications running under Wine, and some terminal
emulators, can behave differently.

**The transcription is poor or empty.**
Make sure the right microphone is selected under **General → Input device**. If the level
bars stay flat, check your system sound settings. If the input is very loud, lower the input
volume in the sound settings, since clipping reduces accuracy. For languages other than
English, try a Whisper model and set the language explicitly.

**The first dictation is slow.**
The model loads when Vaani starts, which takes a few seconds (the Overview page shows
*Loading model*). A dictation made before loading finishes is not transcribed, and a
notification says so. Wait until the status shows *Ready*.

**The tray icon is missing.**
Your desktop needs a StatusNotifierItem host. Cinnamon, KDE Plasma, MATE and Xfce include one.
On GNOME, install the AppIndicator extension. Vaani works without the tray icon; use the
application menu or `vaani settings` instead.

**`vaani: command not found`.**
`~/.local/bin` is not on your `PATH` yet. Log out and back in, or run `~/.local/bin/vaani`.

**`vaani doctor` prints ALSA or JACK messages.**
These come from the audio library while it lists devices. They can be ignored.

For more detail, start Vaani with debug logging and reproduce the problem:

```sh
vaani quit
VAANI_LOG=debug vaani run
```

## Updating and uninstalling

To update, pull the latest changes and run the install script again. Settings, history and
models are kept.

```sh
cd Vaani
git pull
./scripts/install.sh
```

To uninstall:

```sh
./scripts/install.sh --uninstall
```

This removes the binaries, menu entry, icon, fonts and autostart entry. To also remove your
settings, history and models:

```sh
rm -rf ~/.config/vaani ~/.local/share/vaani ~/.local/state/vaani
```

## Building from source

Requirements: Rust 1.85 or later and the dependencies listed under
[Manual dependency installation](#manual-dependency-installation).

```sh
cargo build --release
./target/release/vaani run
```

The sherpa-onnx build script downloads a prebuilt static library from GitHub on the first
build. To build offline, download the archive yourself and set `SHERPA_ONNX_ARCHIVE_DIR` to
the directory that contains it.

Before submitting changes, run the same checks as CI:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Pull requests are welcome. For larger changes, please open an issue first to discuss the
approach.

## Project layout

```
crates/
  vaani-app        The vaani binary: background service, tray, indicator, settings
                   window, D-Bus interface and command line
  vaani-worker     Speech recognition process (sherpa-onnx)
  vaani-audio      Microphone capture and resampling
  vaani-stt        Local and cloud recognition engines
  vaani-hotkey     Global shortcut (X11 XInput2)
  vaani-inject     Text insertion (clipboard and XTest)
  vaani-text       Text clean-up
  vaani-store      History (SQLite) and API keys (Secret Service)
  vaani-models     Model catalogue and downloads
  vaani-core       Configuration, state machine and paths
  vaani-platform   Session and desktop detection
models/            Model catalogue (registry.toml)
assets/            Icon, desktop entry and fonts
scripts/           Install script
```

Vaani runs as a single background process that owns the shortcut, microphone, clipboard and
user interface. Speech recognition runs in `vaani-worker`, a child process, so that a model
failure cannot take down the main application. The command line talks to the running
instance over D-Bus (`io.github.sakshamm21.Vaani`).

## Roadmap

- Wayland support (GNOME, KDE Plasma, Hyprland, Sway).
- Optional AI clean-up of punctuation and formatting, with local or cloud language models.
- More cloud providers.
- Packages: `.deb`, Flatpak and AppImage.

## License

Vaani is licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option. The bundled Inter font is licensed under the SIL Open Font License 1.1; see
`assets/fonts/OFL-Inter.txt`.
