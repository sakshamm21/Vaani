# Platform notes

Findings from early platform experiments (spikes, since removed from the repo). Each entry says what was tried, where, and what happened.
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
- Mint (2026-11-02, simulated with xdotool through XTest): same result. Held 1037 ms and
  320 ms, Esc seen. Auto-repeat filtering still needs a real held key (XTest sends no repeats).

## mic-capture

- Builds against ALSA (`libasound2-dev`); cpal 0.18 takes the `StreamConfig` by value.
- Container has no audio device, so nothing captured there.
- Mint: cpal uses the ALSA host; the default device routes to PipeWire. Native format F32,
  48 kHz, stereo. Mic open to first callback: 23 ms, so a short pre-roll buffer is enough.
  Resampling 4 s from 48 kHz to 16 kHz took 3 ms. The test capture peaked at 0 dBFS (clipped),
  so the onboarding mic check should warn about input gain. `--list` prints many ALSA plugin
  pseudo-devices and JACK noise; the real device picker must filter these.

## parakeet-wav

- sherpa-onnx 2.0.1's build script downloads a prebuilt static library (22 MB) from GitHub.
  Behind a TLS-intercepting proxy, pre-download it and set `SHERPA_ONNX_ARCHIVE_DIR`.
- Model: `sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8`, 640 MB on disk (encoder 622 MB).
- Container (4 vCPU): load and decode a 3.8 s English clip in about 0.4 s with 4 threads,
  real-time factor 0.105 to 0.110, so a 10 s clip would take about 1.1 s. RSS about 840 MB.
  Text was exact. Laptop CPUs should be faster per core; M1 latency target (700 ms for 10 s)
  may need progressive decoding while the user is still talking.
- Mint (Ryzen 5 5600U, 6 cores / 12 threads, 14 GB RAM): model load 3.8 s, RSS about 800 MB.
  4 threads: 0.41 s for the 3.85 s clip (RTF 0.106), text exact. 12 threads was more than
  twice as slow (RTF 0.24), so default to at most the physical core count, and benchmark.
  A 4 s mic capture decoded in 0.39 s.
- Decisions for M1: keep the model loaded in the worker (3.8 s load is too slow per use);
  10 s of audio would take about 1 s, which misses the 700 ms target, so decode in chunks
  while the key is held and only finish the tail on release.

## x11-clipboard-paste

- Container (with `xclip` as the reading app): we take CLIPBOARD, the reader asks for
  `UTF8_STRING`, we serve it, then restore the saved old clipboard, which the next reader gets
  back intact (Unicode included). We can tell exactly when the app has read our text, so the
  restore never has to guess with a fixed delay.
- X11 clipboards live in the owning process: the restored clipboard only survives while Vaani
  runs, which is fine for the always-on app.
- Large clipboard items (INCR protocol) are not handled in the spike; M1 must handle them.
- Mint, pasting into xed: the text including ✓ landed. The app read our text 20 ms after
  Ctrl+V and made about a dozen requests (TARGETS several times, UTF8_STRING,
  text/plain;charset=utf-8, and GTK_TEXT_BUFFER_CONTENTS, which we refuse). Waiting 150 ms after
  the first read covered them all. The saved clipboard (7 targets, including Chromium's
  private ones) was restored and read back 3 times.
- Still to test by hand: Firefox, LibreOffice Writer, VS Code, and a terminal with `--terminal`.

## x11-overlay

- Container with openbox: `--mode override` (override-redirect) kept keyboard focus and the
  active window unchanged for the whole run. `--mode hints` (notification type, input=false)
  **stole focus** within 170 ms. Conclusion so far: use override-redirect on X11.
- GTK4 4.14 on X11: we get the X window id from `gdk4_x11::X11Surface`, set override-redirect
  in `realize` (before map) and position the window ourselves after map.
- Mint (Muffin): identical. Override-redirect: focus never changed. Hints: focus stolen in
  89 ms. **Decision: override-redirect for the X11 overlay.**
