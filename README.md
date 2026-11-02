# Vaani

Linux-first, open-source dictation: hold a hotkey, speak, release, and clean text lands in
whatever app has focus. Runs a local model by default, or your own API key. No account, no
server, no telemetry.

> **Status: early but usable on Linux Mint and other X11 desktops.** Wayland comes later.

## Install (Mint, Ubuntu, Debian, Pop!_OS)

```sh
git clone https://github.com/sakshamm21/Vaani.git
cd Vaani
./scripts/install.sh
```

The script installs the build packages it needs (asks for your password once), builds Vaani,
puts it in `~/.local/bin`, adds it to your app menu and starts it. The first time, Vaani opens
its window and offers to download a speech model (Parakeet, about 640 MB, runs offline).

Then, in any app: **hold Right Ctrl, speak, let go.** Tap Right Ctrl for hands-free; tap again
to finish. Esc cancels. Change the key, mic, model, theme (light or dark) and more from the
tray icon or the app menu.

Remove it with `./scripts/install.sh --uninstall` (your settings and models stay).

## Light on your machine

A native GTK4 app, no browser engine. Idle in the tray it uses about 0.1% of one CPU core and
about 40 MB of its own memory (the rest is shared GTK libraries). The speech model is the big
part: Parakeet keeps about 800 MB in memory so dictation is instant. Pick a smaller model
(Moonshine, Whisper tiny/base) on the Speech model page if you need the memory.

## Commands

| Command | What it does |
| --- | --- |
| `vaani` / `vaani run` | Start the app (only one copy runs at a time) |
| `vaani settings` | Open the window (starts the app if needed) |
| `vaani paste-last` | Paste the last dictation again |
| `vaani models [list\|download ID --use\|remove ID]` | Manage speech models |
| `vaani history [N]` | Print recent dictations |
| `vaani toggle` | Start dictating, or finish if already dictating |
| `vaani start` / `vaani stop` | Start or finish dictating |
| `vaani cancel` | Throw away the current dictation |
| `vaani status` | Print `idle`, `recording` or `processing` |
| `vaani quit` | Stop the app |
| `vaani doctor [--json]` | Report the session, config, logs and components, for bug reports |

Any desktop's custom-shortcut settings can bind a key to `vaani toggle`.

## Files

- Config: `~/.config/vaani/config.toml` (plain TOML, reloaded when you save it)
- Logs: `~/.local/state/vaani/logs/` (daily files, last 7 kept). Set `VAANI_LOG=debug` for more.

## Supported desktops

| Desktop | Status |
| --- | --- |
| Linux Mint (Cinnamon, MATE, Xfce), Ubuntu or other distros on X11 | Works |
| GNOME, KDE, Hyprland, Sway on Wayland | Planned |

## Layout

```
crates/
  vaani-core/      state machine, config schema, paths
  vaani-platform/  session probe
  vaani-app/       the `vaani` binary: daemon, tray, pill, settings window, D-Bus, CLI
  vaani-worker/    speech-to-text process (sherpa-onnx), kept separate so a crash can't take the app down
  vaani-audio/     microphone capture and resampling
  vaani-stt/       local and cloud engines
  vaani-hotkey/    global key (X11 XInput2)
  vaani-inject/    paste into the focused app (clipboard + XTest)
  vaani-text/      clean-up (fillers, capitals, spacing)
  vaani-store/     history (SQLite) and API keys (system keyring)
  vaani-models/    model catalogue and downloads
assets/            icon, app-menu entry, fonts (Unbounded, Plus Jakarta Sans; SIL OFL)
spikes/            throwaway platform experiments
docs/              platform notes
```

## Licence

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
