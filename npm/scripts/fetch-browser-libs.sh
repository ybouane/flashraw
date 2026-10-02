#!/bin/bash
# Fetch the shared libraries Playwright's headless Chromium needs on a minimal Debian/Ubuntu
# host *without root*: downloads the .deb packages and extracts them into a cache directory.
# (With root, `npx playwright install --with-deps chromium` is the normal route.)
#
#   bash scripts/fetch-browser-libs.sh            # prints the library directory
#   FLASHRAW_BROWSER_LIBS=<dir> npm run test:browser
set -euo pipefail
DEST="${FLASHRAW_BROWSER_LIBS_CACHE:-$HOME/.cache/flashraw-browser-libs}"
mkdir -p "$DEST/debs" "$DEST/root"
cd "$DEST/debs"
apt-get download libnspr4 libnss3 libatk1.0-0t64 libatk-bridge2.0-0t64 libxcomposite1 libxdamage1 \
  libxfixes3 libxrandr2 libgbm1 libxkbcommon0 libasound2t64 libatspi2.0-0t64 libwayland-server0 \
  libdrm2 libxi6 libxtst6 libcups2t64 libxshmfence1 >/dev/null
for d in ./*.deb; do dpkg-deb -x "$d" "$DEST/root"; done
echo "$DEST/root/usr/lib/x86_64-linux-gnu"
