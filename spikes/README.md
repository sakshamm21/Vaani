# Spikes

Small throwaway programs that prove a platform API works before real code depends on it.
They are separate Cargo projects (not workspace members, not built by CI). Findings go in
[`docs/platform-notes.md`](../docs/platform-notes.md).

M0 spikes, all for Linux Mint Cinnamon on X11:

| Spike | What it proves | How to run it |
| --- | --- | --- |
| [`x11-hotkey-hold`](x11-hotkey-hold) | XInput2 raw events see a hotkey go down and up globally, without grabbing it, ignoring auto-repeat | `cargo run -- Control_R`, then hold and release Right Ctrl a few times; press Esc while holding |
| [`mic-capture`](mic-capture) | cpal captures the default mic; rubato resamples to 16 kHz mono | `cargo run --release -- 5` and speak; listen to `capture-16k.wav` |
| [`parakeet-wav`](parakeet-wav) | sherpa-onnx runs Parakeet TDT 0.6B v3 on CPU; load time, RAM, speed | `./fetch-model.sh` once, then `cargo run --release -- ../mic-capture/capture-16k.wav` |
| [`x11-clipboard-paste`](x11-clipboard-paste) | Own CLIPBOARD, paste with XTest Ctrl+V, know when the app read it, restore the old clipboard | Copy something, run `cargo run -- "hello"`, click into a text field within 3 s; paste by hand afterwards to check the old clipboard is back |
| [`x11-overlay`](x11-overlay) | A GTK4 pill floats on top without taking keyboard focus | Click into an editor, run `sleep 3; cargo run -- --mode override`, keep typing; repeat with `--mode hints` |
