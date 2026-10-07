#!/usr/bin/env bash
# Run the local CI (tools/ci/local.sh) on a macOS host over SSH.
#
#   tools/ci/mac.sh [--keep] [local.sh arguments...]
#
# Copies the working tree (without target/, corpus/, node_modules/, reference/) and a small
# corpus sample to ~/flashraw-ci on the host, fetches portable CMake and Node.js into that
# directory when the host has none (no system-wide installs), runs local.sh there and
# deletes ~/flashraw-ci afterwards unless --keep is given. Configure the host with:
#   FLASHRAW_MAC_SSH   ssh destination and options, e.g. "-p 2222 -i key.pem user@host"
#   FLASHRAW_MAC_SAMPLE  corpus sample filter (regex on corpus paths; default below)

set -euo pipefail
cd "$(dirname "$0")/../.."
SSH_ARGS=(${FLASHRAW_MAC_SSH:?set FLASHRAW_MAC_SSH, e.g. \"-p 2222 -i key.pem user@host\"})
KEEP=0
if [ "${1:-}" = "--keep" ]; then KEEP=1; shift; fi
SAMPLE="${FLASHRAW_MAC_SAMPLE:-Leica/|Pentax/K-70|Blackmagic|Canon/EOS R5/|Nikon/Z 7/|Sony/ILCE-7M4/|Fujifilm/X-T3/|Panasonic/DC-GH5/|Olympus/E-M1MarkII/}"
DIR=flashraw-ci
RSH="ssh ${SSH_ARGS[*]:0:${#SSH_ARGS[@]}-1}"
HOST="${SSH_ARGS[-1]}"

remote() { ssh "${SSH_ARGS[@]}" "$@"; }

echo "=== syncing tree to $HOST:~/$DIR"
remote "mkdir -p ~/$DIR/src ~/$DIR/corpus"
rsync -a --delete -e "$RSH" --exclude target --exclude corpus --exclude reference --exclude node_modules \
  --exclude .claude --exclude '*.whl' ./ "$HOST:$DIR/src/"
if [ -d corpus ]; then
  echo "=== syncing corpus sample"
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
  FLASHRAW_CORPUS=\$HOME/$DIR/corpus && tools/ci/local.sh $*" || status=$?

mkdir -p target/ci-logs-mac
rsync -a -e "$RSH" "$HOST:$DIR/src/target/ci-logs/" target/ci-logs-mac/ || true
echo "=== logs copied to target/ci-logs-mac/"

if [ "$KEEP" = 0 ]; then
  echo "=== removing ~/$DIR on $HOST"
  remote "rm -rf ~/$DIR"
fi
remote "df -h ~ | tail -1"
exit $status
