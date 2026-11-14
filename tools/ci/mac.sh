#!/usr/bin/env bash
# Run the local CI (tools/ci/local.sh) on a macOS host over SSH.
#
#   tools/ci/mac.sh [--clean] [local.sh arguments...]
#
# Keeps a warm work directory ~/flashraw-ci on the host (build cache, npm modules, the
# wasm-bindgen tool, the Python venv and a small corpus sample), so repeat runs only rebuild
# what changed. Each run syncs the working tree (without target/, corpus/, node_modules/,
# reference/), fetches portable CMake and Node.js into the work directory when the host has
# none (no system-wide installs), runs local.sh there and copies the logs back to
# target/ci-logs-mac/. The build cache is dropped when the host has less than
# FLASHRAW_MAC_MIN_FREE_GB (default 10) GB free; --clean removes ~/flashraw-ci entirely.
#   FLASHRAW_MAC_SSH     ssh destination and options, e.g. "-p 2222 -i key.pem user@host"
#   (the Mac holds only a corpus sample, so the regression test runs with FLASHRAW_ALLOW_NO_CORPUS=1)
#   FLASHRAW_MAC_SAMPLE  corpus sample filter (regex on corpus paths; default below)

set -euo pipefail
cd "$(dirname "$0")/../.."
SSH_ARGS=(${FLASHRAW_MAC_SSH:?set FLASHRAW_MAC_SSH, e.g. \"-p 2222 -i key.pem user@host\"})
MIN_FREE_GB="${FLASHRAW_MAC_MIN_FREE_GB:-10}"
SAMPLE="${FLASHRAW_MAC_SAMPLE:-Leica/|Pentax/K-70|Blackmagic|Canon/EOS R5/|Nikon/Z 7/|Sony/ILCE-7M4/|Fujifilm/X-T3/|Panasonic/DC-GH5/|Olympus/E-M1MarkII/}"
DIR=flashraw-ci
RSH="ssh ${SSH_ARGS[*]:0:${#SSH_ARGS[@]}-1}"
HOST="${SSH_ARGS[-1]}"
remote() { ssh "${SSH_ARGS[@]}" "$@"; }

if [ "${1:-}" = "--clean" ]; then
  shift
  echo "=== removing ~/$DIR on $HOST"
  remote "rm -rf ~/$DIR"
  [ $# -eq 0 ] && exit 0
fi

# Free disk (GB) on the host's home volume.
free_gb() { remote "df -g ~ | awk 'NR==2 {print \$4}'"; }
if [ "$(free_gb)" -lt "$MIN_FREE_GB" ]; then
  echo "=== less than $MIN_FREE_GB GB free on $HOST: dropping the build cache"
  remote "rm -rf ~/$DIR/src/target"
fi

echo "=== syncing tree to $HOST:~/$DIR (warm cache kept)"
remote "mkdir -p ~/$DIR/src ~/$DIR/corpus"
# --delete keeps the copy exact, except for the caches the exclusions protect.
rsync -a --delete -e "$RSH" --exclude /target --exclude /corpus --exclude /reference \
  --exclude node_modules --exclude /npm/.tools --exclude /npm/dist --exclude .claude --exclude '*.whl' \
  ./ "$HOST:$DIR/src/"
if [ -d corpus ]; then
  (cd corpus && find . -type f | sed 's|^\./||' | grep -E "$SAMPLE" || true) > target/ci-mac-sample.txt
  rsync -a -e "$RSH" --files-from=target/ci-mac-sample.txt corpus/ "$HOST:$DIR/corpus/"
fi

# Portable CMake and Node.js inside ~/flashraw-ci/tools when the host lacks them.
remote "bash -s" <<'EOF'
set -e
cd ~/flashraw-ci && mkdir -p tools && cd tools
arch=$(uname -m)
if ! command -v cmake >/dev/null && [ ! -x cmake/CMake.app/Contents/bin/cmake ]; then
  curl -fsSL https://github.com/Kitware/CMake/releases/download/v3.31.6/cmake-3.31.6-macos-universal.tar.gz | tar xz
  mv cmake-3.31.6-macos-universal cmake
fi
# Node.js 24 LTS (the tests use `using` declarations; the bundlers need a recent Node).
if ! command -v node >/dev/null && [ ! -x node/bin/node ]; then
  n=$([ "$arch" = arm64 ] && echo arm64 || echo x64)
  f=$(curl -fsSL https://nodejs.org/dist/latest-v24.x/SHASUMS256.txt | grep -o "node-v[0-9.]*-darwin-$n.tar.gz" | head -1)
  curl -fsSL "https://nodejs.org/dist/latest-v24.x/$f" | tar xz
  mv "${f%.tar.gz}" node
fi
EOF

status=0
remote "cd ~/$DIR/src && export PATH=\$HOME/.cargo/bin:\$HOME/$DIR/tools/node/bin:\$HOME/$DIR/tools/cmake/CMake.app/Contents/bin:\$PATH \
  FLASHRAW_CORPUS=\$HOME/$DIR/corpus FLASHRAW_ALLOW_NO_CORPUS=1 && tools/ci/local.sh $*" || status=$?

mkdir -p target/ci-logs-mac
rsync -a -e "$RSH" "$HOST:$DIR/src/target/ci-logs/" target/ci-logs-mac/ || true
echo "=== logs copied to target/ci-logs-mac/"
if [ "$(free_gb)" -lt "$MIN_FREE_GB" ]; then
  echo "=== less than $MIN_FREE_GB GB free on $HOST: dropping the build cache"
  remote "rm -rf ~/$DIR/src/target"
fi
remote "du -sh ~/$DIR | cut -f1 | sed 's/^/work dir: /'; df -h ~ | tail -1"
exit $status
