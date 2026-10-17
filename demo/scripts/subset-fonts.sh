#!/bin/sh
# Regenerates src/fonts/newsreader-core.woff2: Newsreader (roman, opsz + wght axes) cut down to
# the characters the page itself uses, so it can be preloaded cheaply. Everything else in the
# latin range comes from the full @fontsource file (see the @font-face rules in src/styles.css).
#   pip install fonttools brotli && sh scripts/subset-fonts.sh
set -e
cd "$(dirname "$0")/.."
mkdir -p src/fonts
pyftsubset node_modules/@fontsource-variable/newsreader/files/newsreader-latin-opsz-normal.woff2 \
  --unicodes="U+0020-007E,U+00A0,U+00B7,U+00D7,U+2013-2014,U+2018-2019,U+201C-201D,U+2026,U+2190-2193,U+2212,U+2248,U+2265" \
  --layout-features='kern,liga,lnum,tnum' --flavor=woff2 --output-file=src/fonts/newsreader-core.woff2
cp node_modules/@fontsource-variable/newsreader/LICENSE src/fonts/OFL.txt
