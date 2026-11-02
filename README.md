# Vaani

Linux-first, open-source dictation: hold a hotkey, speak, release, and clean text lands in
whatever app has focus. Runs a local model by default, or your own API key. No account, no
server, no telemetry.

> **Status: early development (milestone M0, foundations).** Nothing dictates yet. The app
> starts, answers CLI commands over D-Bus and reports your setup with `vaani doctor`.

## Build and try it

You need Rust 1.85 or newer ([rustup.rs](https://rustup.rs)).

```sh
cargo build --release
./target/release/vaani doctor        # what Vaani sees on this machine
./target/release/vaani run &         # start the background app
./target/release/vaani toggle        # prints: recording
./target/release/vaani status
./target/release/vaani quit
```

## Commands

| Command | What it does |
| --- | --- |
| `vaani` / `vaani run` | Start the app (only one copy runs at a time) |
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
| Linux Mint Cinnamon (X11) | First target (M1) |
| GNOME, KDE, Hyprland, Sway on Wayland | Planned (M3) |

## Layout

```
crates/
  vaani-core/      state machine, config schema, paths
  vaani-platform/  session probe
  vaani-app/       the `vaani` binary: daemon, D-Bus service, CLI, doctor
  vaani-worker/    inference process (stub)
  vaani-audio, vaani-stt, vaani-llm, vaani-text, vaani-hotkey,
  vaani-inject, vaani-store, vaani-models   (stubs, filled in from M1)
spikes/            throwaway platform experiments
docs/              platform notes
```

## Licence

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
