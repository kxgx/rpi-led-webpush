#!/usr/bin/env bash
# Build a .deb for rpi-led-webpush (local or CI).
# Usage: deploy/build-deb.sh [output-dir]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/dist}"
PKG_NAME=rpi-led-webpush
VERSION="$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed 's/.*"\(.*\)"/\1/')"
ARCH="$(dpkg --print-architecture)"
MAINTAINER="kxgx"
DESC="Push video from a browser to a HUB75 LED matrix (zero third-party crates)"

STAGE="$ROOT/target/deb-root"
rm -rf "$STAGE"
mkdir -p "$STAGE/DEBIAN" \
         "$STAGE/usr/bin" \
         "$STAGE/lib/systemd/system" \
         "$STAGE/usr/share/doc/$PKG_NAME" \
         "$STAGE/usr/share/$PKG_NAME" \
         "$STAGE/etc/$PKG_NAME" \
         "$OUT"

echo "==> build release binary"
cargo build --release --manifest-path "$ROOT/Cargo.toml"

echo "==> stage FHS layout"
install -m755 "$ROOT/target/release/rpi-led-webpush" "$STAGE/usr/bin/"
install -m644 "$ROOT/contrib/rpi-led-webpush.service" \
  "$STAGE/lib/systemd/system/rpi-led-webpush.service"
install -m644 "$ROOT/contrib/rpi-led-webpush.conf.example" \
  "$STAGE/etc/$PKG_NAME/config"
install -m644 "$ROOT/contrib/rpi-led-webpush.conf.example" \
  "$STAGE/usr/share/$PKG_NAME/rpi-led-webpush.conf.example"
install -m644 "$ROOT/README.md" "$ROOT/README.zh-CN.md" \
  "$STAGE/usr/share/doc/$PKG_NAME/"
install -m644 "$ROOT/third_party/rpi-rgb-led-matrix/COPYING" \
  "$STAGE/usr/share/doc/$PKG_NAME/COPYING.rpi-rgb-led-matrix"

# systemd unit：二进制装到 /usr/bin；配置由 /etc/rpi-led-webpush/config 提供
# WorkingDirectory 不能指向未打包的 /opt 路径，否则 ExecStart 会 CHDIR 失败
sed -i \
  -e 's#/opt/rpi-led-webpush/rpi-led-webpush#/usr/bin/rpi-led-webpush#g' \
  -e 's#WorkingDirectory=.*#WorkingDirectory=/var/lib/rpi-led-webpush#' \
  -e 's#Settings live in .*#Settings live in /etc/rpi-led-webpush/config (editable at /settings).#' \
  "$STAGE/lib/systemd/system/rpi-led-webpush.service"

cat > "$STAGE/DEBIAN/control" <<EOF
Package: $PKG_NAME
Version: $VERSION
Section: utils
Priority: optional
Architecture: $ARCH
Depends: libc6, libstdc++6
Maintainer: $MAINTAINER
Description: $DESC
 Web UI for sending video / camera / screen share to a HUB75 RGB LED
 matrix panel driven by a Raspberry Pi. Settings, idle clock and live
 brightness are included; the HUB75 driver is vendored and statically linked.
EOF

cat > "$STAGE/DEBIAN/postinst" <<'EOF'
#!/bin/bash
set -e
mkdir -p /etc/rpi-led-webpush /var/lib/rpi-led-webpush
if command -v systemctl >/dev/null 2>&1; then
  systemctl daemon-reload || true
fi
echo "rpi-led-webpush installed."
echo "  config:  /etc/rpi-led-webpush/config"
echo "  start:   sudo systemctl enable --now rpi-led-webpush"
echo "  web ui:  http://<device>:8080/  (camera/screen need HTTPS, see contrib/)"
echo "  upgrade: sudo DEBIAN_FRONTEND=noninteractive dpkg -i --force-confold <deb>  # keep your config"
EOF
chmod 755 "$STAGE/DEBIAN/postinst"

cat > "$STAGE/DEBIAN/prerm" <<'EOF'
#!/bin/bash
set -e
if [ "$1" = remove ] && command -v systemctl >/dev/null 2>&1; then
  systemctl stop rpi-led-webpush.service 2>/dev/null || true
  systemctl disable rpi-led-webpush.service 2>/dev/null || true
fi
EOF
chmod 755 "$STAGE/DEBIAN/prerm"

cat > "$STAGE/DEBIAN/postrm" <<'EOF'
#!/bin/bash
set -e
if command -v systemctl >/dev/null 2>&1; then
  systemctl daemon-reload || true
fi
if [ "$1" = purge ]; then
  rm -rf /etc/rpi-led-webpush
fi
EOF
chmod 755 "$STAGE/DEBIAN/postrm"

# 用户配置升级时不覆盖
echo /etc/rpi-led-webpush/config > "$STAGE/DEBIAN/conffiles"

echo "==> dpkg-deb"
DEB="$OUT/${PKG_NAME}_${VERSION}_${ARCH}.deb"
dpkg-deb --build --root-owner-group "$STAGE" "$DEB"
dpkg-deb -I "$DEB" || true
dpkg-deb -c "$DEB" | head -30
echo "OK: $DEB"
