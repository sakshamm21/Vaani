#!/usr/bin/env bash
# Build release packages into dist/:
#   vaani_<version>_amd64.deb            for Debian, Ubuntu, Mint, Pop!_OS
#   vaani-<version>-1.x86_64.rpm         for Fedora and openSUSE (needs rpmbuild)
#   vaani-<version>-x86_64-linux.tar.gz  the two binaries plus icons and the menu entry
#   PKGBUILD                             for Arch (the AUR's vaani-bin), using the tarball
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
  install -Dm644 assets/70-vaani-uinput.rules "$root$prefix/lib/udev/rules.d/70-vaani-uinput.rules"
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
Depends: libgtk-4-1, libasound2t64 | libasound2, libx11-6, libxi6, libxcb1, libxkbcommon0
Recommends: gnome-keyring | kwalletmanager
Section: utils
Priority: optional
Homepage: https://github.com/sakshamm21/vaani
Description: Voice dictation for Linux
 Hold a key, speak, and your words are typed into any app. Speech recognition
 runs on your computer by default (Parakeet, Whisper, Moonshine, SenseVoice),
 with optional cloud transcription through your own API key. Works on X11 and
 Wayland desktops.
CONTROL
cat > "$DEB/DEBIAN/postinst" <<'POSTINST'
#!/bin/sh
set -e
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t /usr/share/icons/hicolor || true
command -v update-desktop-database >/dev/null && update-desktop-database -q /usr/share/applications || true
command -v udevadm >/dev/null && { udevadm control --reload || true; udevadm trigger --subsystem-match=misc --action=change || true; }
POSTINST
chmod 755 "$DEB/DEBIAN/postinst"
dpkg-deb --root-owner-group --build "$DEB" "$DIST/vaani_${VERSION}_${ARCH}.deb" >/dev/null
rm -rf "$DEB"

# .rpm. Its library requirements are found automatically (by soname), so one package works
# on Fedora and openSUSE, whose package names differ.
if command -v rpmbuild >/dev/null; then
  RPM="$PWD/$DIST/rpmbuild"
  mkdir -p "$RPM/root"
  stage "$RPM/root" /usr
  cat > "$RPM/vaani.spec" <<SPEC
Name: vaani
Version: $VERSION
Release: 1
Summary: Voice dictation for Linux
License: MIT
URL: https://github.com/sakshamm21/vaani
Recommends: gnome-keyring

%description
Hold a key, speak, and your words are typed into any app. Speech recognition runs on
your computer by default, with optional cloud transcription through your own API key.
Works on X11 and Wayland desktops.

%install
cp -a $RPM/root/. %{buildroot}/

%post
udevadm control --reload >/dev/null 2>&1 || :
udevadm trigger --subsystem-match=misc --action=change >/dev/null 2>&1 || :

%files
/usr/bin/vaani
/usr/bin/vaani-worker
/usr/share/applications/$ID.desktop
/usr/share/icons/hicolor/scalable/apps/$ID.svg
/usr/share/icons/hicolor/scalable/apps/$ID-dark.svg
/usr/lib/udev/rules.d/70-vaani-uinput.rules
%license /usr/share/doc/vaani/LICENSE
%license /usr/share/doc/vaani/OFL-Inter.txt
SPEC
  rpmbuild -bb --quiet --target x86_64 \
    --define "_topdir $RPM" --define "_build_id_links none" --define "debug_package %{nil}" \
    "$RPM/vaani.spec"
  mv "$RPM"/RPMS/*/*.rpm "$DIST/"
  rm -rf "$RPM"
else
  echo "rpmbuild not found; skipping the .rpm" >&2
fi

# .tar.gz with an install script for any distribution
TAR="vaani-$VERSION-$(uname -m)-linux"
stage "$DIST/$TAR" ""
cat > "$DIST/$TAR/install.sh" <<'INSTALL'
#!/bin/sh
# Installs Vaani for the current user under ~/.local; `./install.sh --uninstall` removes it.
# Needs GTK 4 and ALSA from your distribution.
set -e
cd "$(dirname "$0")"
ID=io.github.sakshamm21.Vaani
if [ "${1:-}" = "--uninstall" ]; then
  "$HOME/.local/bin/vaani" quit >/dev/null 2>&1 || true
  rm -f "$HOME/.local/bin/vaani" "$HOME/.local/bin/vaani-worker" \
        "$HOME/.local/share/applications/$ID.desktop" "$HOME/.config/autostart/$ID.desktop" \
        "$HOME/.local/share/icons/hicolor/scalable/apps/$ID.svg" \
        "$HOME/.local/share/icons/hicolor/scalable/apps/$ID-dark.svg"
  echo "Vaani removed. Settings, history and models stay in ~/.config/vaani and ~/.local/share/vaani."
  exit 0
fi
mkdir -p "$HOME/.local"
cp -r bin share "$HOME/.local/"
sed -i "s|^Exec=vaani|Exec=$HOME/.local/bin/vaani|" "$HOME/.local/share/applications/$ID.desktop"
gtk-update-icon-cache -q -t "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
echo "Installed. Start Vaani from your app menu, or run: ~/.local/bin/vaani"
if [ ! -w /dev/uinput ]; then
  echo "On Wayland, also run this once so Vaani can press the paste key:"
  echo "  sudo install -m644 lib/udev/rules.d/70-vaani-uinput.rules /etc/udev/rules.d/ && sudo udevadm trigger --subsystem-match=misc --action=change"
fi
INSTALL
chmod 755 "$DIST/$TAR/install.sh"
tar -C "$DIST" -czf "$DIST/$TAR.tar.gz" "$TAR"
rm -rf "${DIST:?}/$TAR"

# PKGBUILD for the AUR's vaani-bin, pointing at the tarball on the GitHub release.
TAR_SUM=$(sha256sum "$DIST/$TAR.tar.gz" | cut -d' ' -f1)
cat > "$DIST/PKGBUILD" <<PKGBUILD
# Maintainer: sakshamm21 <saksham2003malhotra@gmail.com>
pkgname=vaani-bin
pkgver=$VERSION
pkgrel=1
pkgdesc="Voice dictation for Linux: hold a key, speak, and your words are typed into any app"
arch=('x86_64')
url="https://github.com/sakshamm21/vaani"
license=('MIT')
depends=('gtk4' 'alsa-lib' 'libx11' 'libxi' 'libxcb' 'libxkbcommon')
optdepends=('gnome-keyring: store API keys' 'kwallet: store API keys on KDE')
provides=('vaani')
conflicts=('vaani')
source=("https://github.com/sakshamm21/vaani/releases/download/v\$pkgver/$TAR.tar.gz")
sha256sums=('$TAR_SUM')

package() {
  cd "$TAR"
  install -Dm755 bin/vaani bin/vaani-worker -t "\$pkgdir/usr/bin/"
  cp -a share lib "\$pkgdir/usr/"
  install -Dm644 share/doc/vaani/LICENSE "\$pkgdir/usr/share/licenses/\$pkgname/LICENSE"
}
PKGBUILD

(cd "$DIST" && shopt -s nullglob && sha256sum *.deb *.rpm *.tar.gz PKGBUILD > SHA256SUMS)
ls -l "$DIST"
