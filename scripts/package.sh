#!/usr/bin/env bash
# Build release packages into dist/:
#   vaani_<version>_amd64.deb          for Debian, Ubuntu, Mint, Pop!_OS
#   vaani-<version>-x86_64-linux.tar.gz  the two binaries plus icons and the menu entry
#   SHA256SUMS
# Build on the oldest distribution you want to support (CI uses Ubuntu 22.04), because the
# binaries need a glibc at least as new as the one they were built with.
set -euo pipefail

cd "$(dirname "$0")/.."
ID=io.github.sakshamm21.Vaani
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ARCH=$(dpkg --print-architecture 2>/dev/null || echo amd64)
DIST=dist
rm -rf "$DIST"
mkdir -p "$DIST"

cargo build --release --locked -p vaani-app -p vaani-worker

# Files laid out as they are installed, shared by both packages.
stage() {
  local root=$1 prefix=$2
  install -Dm755 target/release/vaani "$root$prefix/bin/vaani"
  install -Dm755 target/release/vaani-worker "$root$prefix/bin/vaani-worker"
  install -Dm644 assets/$ID.desktop "$root$prefix/share/applications/$ID.desktop"
  install -Dm644 assets/icons/$ID.svg "$root$prefix/share/icons/hicolor/scalable/apps/$ID.svg"
  install -Dm644 assets/icons/$ID-dark.svg "$root$prefix/share/icons/hicolor/scalable/apps/$ID-dark.svg"
  install -Dm644 LICENSE "$root$prefix/share/doc/vaani/LICENSE"
  install -Dm644 assets/fonts/OFL-Inter.txt "$root$prefix/share/doc/vaani/OFL-Inter.txt"
}

# .deb
DEB="$DIST/deb"
stage "$DEB" /usr
SIZE=$(du -sk "$DEB/usr" | cut -f1)
mkdir -p "$DEB/DEBIAN"
cat > "$DEB/DEBIAN/control" <<CONTROL
Package: vaani
Version: $VERSION
Architecture: $ARCH
Maintainer: sakshamm21 <saksham2003malhotra@gmail.com>
Installed-Size: $SIZE
Depends: libgtk-4-1, libasound2t64 | libasound2, libx11-6, libxi6, libxcb1
Recommends: gnome-keyring | kwalletmanager
Section: utils
Priority: optional
Homepage: https://github.com/sakshamm21/vaani
Description: Voice dictation for Linux
 Hold a key, speak, and your words are typed into any app. Speech recognition
 runs on your computer by default (Parakeet, Whisper, Moonshine, SenseVoice),
 with optional cloud transcription through Groq, OpenAI or a compatible server.
CONTROL
cat > "$DEB/DEBIAN/postinst" <<'POSTINST'
#!/bin/sh
set -e
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t /usr/share/icons/hicolor || true
command -v update-desktop-database >/dev/null && update-desktop-database -q /usr/share/applications || true
POSTINST
chmod 755 "$DEB/DEBIAN/postinst"
dpkg-deb --root-owner-group --build "$DEB" "$DIST/vaani_${VERSION}_${ARCH}.deb" >/dev/null
rm -rf "$DEB"

# .tar.gz with an install script for any distribution
TAR="vaani-$VERSION-$(uname -m)-linux"
stage "$DIST/$TAR" ""
cat > "$DIST/$TAR/install.sh" <<'INSTALL'
#!/bin/sh
# Installs Vaani for the current user under ~/.local. Needs GTK 4 and ALSA from your distribution.
set -e
cd "$(dirname "$0")"
mkdir -p "$HOME/.local"
cp -r bin share "$HOME/.local/"
sed -i "s|^Exec=vaani|Exec=$HOME/.local/bin/vaani|" "$HOME/.local/share/applications/io.github.sakshamm21.Vaani.desktop"
gtk-update-icon-cache -q -t "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
echo "Installed. Start Vaani from your app menu, or run: ~/.local/bin/vaani"
INSTALL
chmod 755 "$DIST/$TAR/install.sh"
tar -C "$DIST" -czf "$DIST/$TAR.tar.gz" "$TAR"
rm -rf "${DIST:?}/$TAR"

(cd "$DIST" && sha256sum ./*.deb ./*.tar.gz | sed 's| \./| |' > SHA256SUMS)
ls -l "$DIST"
