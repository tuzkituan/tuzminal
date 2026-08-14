#!/usr/bin/env bash
#
# Build a distributable release archive.
#
# Produces dist/tuzminal-<version>-<arch>-<os>.tar.gz containing a stripped binary,
# the desktop entry and icon, the docs, the example plugins, and an install script —
# everything someone needs without a Rust toolchain.
#
# The binary is stripped here rather than in the release profile: that profile keeps
# debug symbols on purpose so perf profiles stay readable, and they take the binary
# from 25 MB to 167 MB. A download should not carry them.

set -euo pipefail

cd "$(dirname "$0")/.."

VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
ARCH=$(uname -m)
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
NAME="tuzminal-${VERSION}-${ARCH}-${OS}"

DIST="dist"
STAGE="${DIST}/${NAME}"

echo "==> building ${NAME}"
cargo build --release --locked

rm -rf "${STAGE}"
mkdir -p "${STAGE}/share"

cp target/release/tuzminal "${STAGE}/tuzminal"
strip "${STAGE}/tuzminal" 2>/dev/null || echo "    (strip unavailable; shipping unstripped)"

cp README.md LICENSE-MIT LICENSE-APACHE "${STAGE}/"
cp docs/PLUGINS.md "${STAGE}/share/"
cp assets/tuzminal.svg "${STAGE}/share/"
cp -r examples "${STAGE}/share/examples"

# The installer is generated rather than kept as a file: it has to agree with the
# layout above, and two copies of that knowledge would drift.
cat > "${STAGE}/install.sh" <<'INSTALL'
#!/usr/bin/env bash
#
# Install tuzminal for the current user. Nothing here needs root.

set -euo pipefail
cd "$(dirname "$0")"

BIN="${HOME}/.local/bin"
mkdir -p "${BIN}"
install -m755 tuzminal "${BIN}/tuzminal"

# Register with the desktop environment. The binary writes its own entry, so the
# path it records is the one it was installed to.
"${BIN}/tuzminal" --install-desktop-entry

echo
echo "Installed to ${BIN}/tuzminal"
case ":${PATH}:" in
  *":${BIN}:"*) ;;
  *) echo "NOTE: ${BIN} is not on your PATH. Add it to your shell profile." ;;
esac
echo "Uninstall with ./uninstall.sh"
INSTALL
chmod +x "${STAGE}/install.sh"

cat > "${STAGE}/uninstall.sh" <<'UNINSTALL'
#!/usr/bin/env bash
#
# Remove tuzminal. Settings, themes and plugins are left alone unless you say so.

set -euo pipefail

rm -f "${HOME}/.local/bin/tuzminal"
rm -f "${HOME}/.local/share/applications/tuzminal.desktop"
rm -f "${HOME}/.local/share/icons/hicolor/scalable/apps/tuzminal.svg"
echo "Removed the program."

if [ "${1:-}" = "--purge" ]; then
  rm -rf "${HOME}/.config/tuzminal" "${HOME}/.local/share/tuzminal"
  echo "Removed your settings, themes and plugins too."
else
  echo "Your settings, themes and plugins are still in:"
  echo "  ${HOME}/.config/tuzminal"
  echo "  ${HOME}/.local/share/tuzminal"
  echo "Pass --purge to remove those as well."
fi
UNINSTALL
chmod +x "${STAGE}/uninstall.sh"

echo "==> archiving"
tar -czf "${DIST}/${NAME}.tar.gz" -C "${DIST}" "${NAME}"

# Checksums beside the archive, so a download can be verified without a second file
# to find.
( cd "${DIST}" && sha256sum "${NAME}.tar.gz" > "${NAME}.tar.gz.sha256" )

echo
echo "  ${DIST}/${NAME}.tar.gz"
ls -lh "${DIST}/${NAME}.tar.gz" | awk '{print "  size: " $5}'
cat "${DIST}/${NAME}.tar.gz.sha256" | awk '{print "  sha256: " $1}'
