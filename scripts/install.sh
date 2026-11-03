#!/usr/bin/env bash
# Build Vaani and install it for the current user (no sudo needed for this part).
#
#   ./scripts/install.sh            build, install, start
#   ./scripts/install.sh --uninstall
#
# Everything goes under ~/.local: binaries in bin/, icon, fonts and the app-menu entry in share/.
set -euo pipefail

cd "$(dirname "$0")/.."
BIN="$HOME/.local/bin"
SHARE="$HOME/.local/share"
ID=io.github.sakshamm21.Vaani

# Root (as in a container) needs no sudo.
SUDO=sudo
[[ $(id -u) -eq 0 ]] && SUDO=

say() { printf '\033[1;35m▸\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!\033[0m %s\n' "$*"; }

if [[ "${1:-}" == "--uninstall" ]]; then
  "$BIN/vaani" quit >/dev/null 2>&1 || true
  rm -f "$BIN/vaani" "$BIN/vaani-worker" "$SHARE/applications/$ID.desktop" \
        "$SHARE/icons/hicolor/scalable/apps/$ID.svg" "$SHARE/icons/hicolor/scalable/apps/$ID-dark.svg" \
        "$HOME/.config/autostart/$ID.desktop"
  rm -rf "$SHARE/fonts/vaani"
  [[ -e /etc/udev/rules.d/70-vaani-uinput.rules ]] && $SUDO rm -f /etc/udev/rules.d/70-vaani-uinput.rules
  say "Vaani removed. Your settings, history and models are still in ~/.config/vaani and ~/.local/share/vaani."
  exit 0
fi

# 1. Build dependencies (Debian, Ubuntu, Mint, Pop!_OS and friends).
# Build packages, by package manager.
if command -v apt-get >/dev/null; then
  need=()
  for pkg in build-essential pkg-config clang cmake libasound2-dev libgtk-4-dev \
             libx11-dev libxi-dev libxtst-dev libxcb1-dev libxkbcommon-dev; do
    dpkg -s "$pkg" >/dev/null 2>&1 || need+=("$pkg")
  done
  if ((${#need[@]})); then
    say "Installing build packages: ${need[*]}"
    $SUDO apt-get install -y "${need[@]}"
  fi
elif command -v dnf >/dev/null; then
  pkgs=(gcc gcc-c++ make pkgconf clang cmake alsa-lib-devel gtk4-devel libX11-devel
        libXi-devel libXtst-devel libxcb-devel libxkbcommon-devel)
  rpm -q "${pkgs[@]}" >/dev/null 2>&1 || { say "Installing build packages"; $SUDO dnf install -y "${pkgs[@]}"; }
elif command -v pacman >/dev/null; then
  pkgs=(base-devel pkgconf clang cmake alsa-lib gtk4 libx11 libxi libxtst libxcb libxkbcommon)
  pacman -T "${pkgs[@]}" >/dev/null 2>&1 || { say "Installing build packages"; $SUDO pacman -S --needed --noconfirm "${pkgs[@]}"; }
elif command -v zypper >/dev/null; then
  pkgs=(gcc gcc-c++ make pkgconf clang cmake alsa-devel gtk4-devel libX11-devel libXi-devel
        libXtst-devel libxcb-devel libxkbcommon-devel)
  rpm -q "${pkgs[@]}" >/dev/null 2>&1 || { say "Installing build packages"; $SUDO zypper --non-interactive install "${pkgs[@]}"; }
else
  warn "Unknown package manager. Please install a C/C++ toolchain, clang, cmake, pkg-config and the development files for ALSA, GTK 4, X11 (Xi, Xtst, xcb) and xkbcommon."
fi
if ! command -v cargo >/dev/null; then
  [[ -f "$HOME/.cargo/env" ]] && source "$HOME/.cargo/env"
fi
if ! command -v cargo >/dev/null; then
  say "Installing Rust (rustup, user-local)"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  source "$HOME/.cargo/env"
fi

# 2. Build.
say "Building (the first build takes a few minutes)"
cargo build --release -p vaani-app -p vaani-worker

# 3. Install.
"$BIN/vaani" quit >/dev/null 2>&1 || true
sleep 0.5
mkdir -p "$BIN" "$SHARE/applications" "$SHARE/icons/hicolor/scalable/apps" "$SHARE/fonts/vaani"
install -m755 target/release/vaani target/release/vaani-worker "$BIN/"
install -m644 assets/icons/$ID.svg assets/icons/$ID-dark.svg "$SHARE/icons/hicolor/scalable/apps/"
rm -f "$SHARE/fonts/vaani/"*.ttf
install -m644 assets/fonts/*.ttf "$SHARE/fonts/vaani/"
sed "s|^Exec=vaani|Exec=$BIN/vaani|" assets/$ID.desktop > "$SHARE/applications/$ID.desktop"
fc-cache -f "$SHARE/fonts/vaani" >/dev/null 2>&1 || true
gtk-update-icon-cache -q -t "$SHARE/icons/hicolor" >/dev/null 2>&1 || true
update-desktop-database -q "$SHARE/applications" >/dev/null 2>&1 || true

# Wayland: the paste key goes through /dev/uinput, which needs a udev rule on some distros.
if [[ ! -w /dev/uinput ]] && [[ ! -e /etc/udev/rules.d/70-vaani-uinput.rules ]]; then
  say "Allowing Vaani to press keys on Wayland (udev rule for /dev/uinput)"
  $SUDO install -m644 assets/70-vaani-uinput.rules /etc/udev/rules.d/70-vaani-uinput.rules &&
    $SUDO udevadm control --reload && $SUDO udevadm trigger --subsystem-match=misc --action=change ||
    warn "Couldn't add the udev rule; on Wayland, Vaani will leave text on the clipboard instead."
fi

case ":$PATH:" in
  *":$BIN:"*) ;;
  *) warn "$BIN is not on your PATH yet; it will be after you log out and back in." ;;
esac

# 4. Start. Vaani adds itself to autostart (you can turn that off in Settings > General).
say "Starting Vaani"
setsid "$BIN/vaani" run >/dev/null 2>&1 < /dev/null &
sleep 1.5
"$BIN/vaani" settings >/dev/null 2>&1 || true
say "Done. Vaani is in your app menu and system tray. Hold Right Ctrl and speak."
