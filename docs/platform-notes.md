# Platform notes

Findings from the spikes in `spikes/`. Each entry says what was tried, where, and what happened.
"Container" means a headless Ubuntu 24.04 VM with Xvfb (and openbox as a window manager where
noted); "Mint" means the owner's laptop (Linux Mint 22.3 Cinnamon, X11).

## Session (from `vaani doctor` on Mint, 2026-11-02)

- X11 session, `XDG_CURRENT_DESKTOP=X-Cinnamon`, D-Bus session bus present.
- `/dev/uinput` is writable through an ACL; the user is not in the `input` group, so evdev
  devices are not readable (fine: XInput2 is the X11 hotkey path; evdev stays opt-in).
- `ibus` and `xdotool` are installed; `wl-copy`, `wtype`, `ydotool` are not (not needed on X11).

## x11-hotkey-hold

- Container: XInput 2.2. Raw key press/release events arrive on the root window from all
  master devices with no grab. Hold durations measured correctly (846 ms and 314 ms holds),
  Esc during a hold is seen. XTest-generated keys also produce raw events.
- Auto-repeat shows up as extra presses with the `KEY_REPEAT` flag; we ignore any press while
  the key is already down.
- Mint: _pending_.

## mic-capture

- Builds against ALSA (`libasound2-dev`); cpal 0.18 takes the `StreamConfig` by value.
- Container has no audio device, so nothing captured there.
- Mint: _pending_ (device name, native rate, mic-open-to-first-callback latency).

## parakeet-wav

- sherpa-onnx 2.0.1's build script downloads a prebuilt static library (22 MB) from GitHub.
  Behind a TLS-intercepting proxy, pre-download it and set `SHERPA_ONNX_ARCHIVE_DIR`.
- Model: `sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8`, 640 MB on disk (encoder 622 MB).
- Container (4 vCPU): load and decode a 3.8 s English clip in about 0.4 s with 4 threads,
  real-time factor 0.105 to 0.110, so a 10 s clip would take about 1.1 s. RSS about 840 MB.
  Text was exact. Laptop CPUs should be faster per core; M1 latency target (700 ms for 10 s)
  may need progressive decoding while the user is still talking.
- Mint: _pending_.

## x11-clipboard-paste

- Container (with `xclip` as the reading app): we take CLIPBOARD, the reader asks for
  `UTF8_STRING`, we serve it, then restore the saved old clipboard, which the next reader gets
  back intact (Unicode included). We can tell exactly when the app has read our text, so the
  restore never has to guess with a fixed delay.
- X11 clipboards live in the owning process: the restored clipboard only survives while Vaani
  runs, which is fine for the always-on app.
- Large clipboard items (INCR protocol) are not handled in the spike; M1 must handle them.
- Mint: _pending_ (real paste into Firefox, Writer, VS Code, terminal with `--terminal`).

## x11-overlay

- Container with openbox: `--mode override` (override-redirect) kept keyboard focus and the
  active window unchanged for the whole run. `--mode hints` (notification type, input=false)
  **stole focus** within 170 ms. Conclusion so far: use override-redirect on X11.
- GTK4 4.14 on X11: we get the X window id from `gdk4_x11::X11Surface`, set override-redirect
  in `realize` (before map) and position the window ourselves after map.
- Mint (Muffin): _pending_.
