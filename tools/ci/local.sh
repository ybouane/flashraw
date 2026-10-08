#!/usr/bin/env bash
# Local CI: the checks of the former hosted workflows, run on this machine.
#
#   tools/ci/local.sh [--changed [BASE] | --quick | --full] [--only s,..] [--skip s,..] [--list]
#
#   --changed [BASE]  (default) run the steps the changes since BASE need: committed changes
#                     since BASE (default origin/main) plus uncommitted and untracked files
#   --quick           the core steps whatever changed (Rust lints and tests, corpus,
#                     camera database, WASM crate)
#   --full            every step, including MSRV, the npm package in browsers and bundlers,
#                     fuzzing and benchmark compilation (before a release)
#   --only / --skip   restrict the selected steps; --list prints them without running
#
# Steps continue past failures; a summary is printed at the end and the exit status is
# non-zero if any step failed. A step whose tools are missing is reported as SKIP (never as
# a pass). Environment:
#   FLASHRAW_CORPUS  corpus root for the regression/malformed suites (default: ./corpus)
#   CMAKE, CTEST     CMake binaries (default: cmake / ctest on PATH)
#   PYTHON_VENV      virtualenv for the Python bindings (default: target/ci-venv, created)

set -uo pipefail
cd "$(dirname "$0")/../.."
ROOT="$PWD"
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}"
export FLASHRAW_CORPUS="${FLASHRAW_CORPUS:-$ROOT/corpus}"
CMAKE="${CMAKE:-cmake}"
CTEST="${CTEST:-ctest}"
MODE=changed
BASE=origin/main
ONLY=""
SKIP=""
LIST=0
while [ $# -gt 0 ]; do
  case "$1" in
    --changed) MODE=changed; if [ $# -gt 1 ] && [[ "$2" != --* ]]; then BASE="$2"; shift; fi ;;
    --quick) MODE=quick ;;
    --full) MODE=full ;;
    --only) ONLY=",$2,"; shift ;;
    --skip) SKIP=",$2,"; shift ;;
    --list) LIST=1 ;;
    -h|--help) sed -n '2,22p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

ALL="fmt clippy test-debug test-release no-default-features corpus msrv camera-db c-header gpu c-api c-api-pkgconfig wasm npm npm-browser npm-bundlers python fuzz benches"
QUICK="fmt clippy test-debug test-release corpus camera-db wasm"

# --- Step selection -----------------------------------------------------------------------
select_changed() {
  local files
  files=$( { git diff --name-only "$BASE"...HEAD 2>/dev/null || git diff --name-only "$BASE" 2>/dev/null;
             git diff --name-only HEAD; git ls-files --others --exclude-standard; } | sort -u)
  if [ -z "$files" ]; then echo ""; return; fi
  local s=" "
  add() { for x in "$@"; do [[ "$s" == *" $x "* ]] || s="$s$x "; done; }
  local rust_core="fmt clippy test-debug test-release corpus"
  while IFS= read -r f; do
    case "$f" in
      *.md|LICENSE*|docs/*|benches/results/*) ;;   # documentation and recorded results
      Cargo.toml|Cargo.lock|*/Cargo.toml|rust-toolchain*)
        add $rust_core no-default-features msrv wasm python c-api c-api-pkgconfig c-header ;;
      crates/flashraw-core/*|crates/flashraw-codecs/*|crates/flashraw-formats/*)
        add $rust_core no-default-features wasm ;;
      crates/flashraw/*)
        add $rust_core no-default-features wasm python c-api c-header ;;
      crates/flashraw-camera-db/*|camera-db/*|tools/support_matrix.py|docs/support*)
        add $rust_core camera-db ;;
      crates/flashraw-gpu/*|shaders/*) add $rust_core gpu wasm npm ;;
      crates/flashraw-cli/*) add $rust_core ;;
      crates/flashraw-capi/*|examples/*) add fmt clippy test-release c-header c-api c-api-pkgconfig ;;
      crates/flashraw-libraw-compat/*) add fmt clippy test-release ;;
      crates/flashraw-wasm/*) add fmt clippy wasm npm ;;
      npm/*) add npm npm-bundlers ;;
      crates/flashraw-python/*) add fmt clippy python ;;
      fuzz/*) add fuzz ;;
      crates/*/benches/*) add fmt clippy benches ;;
      tools/ci/*|tools/corpus/*|tests/*) add $rust_core ;;
      *) add $rust_core ;;
    esac
  done <<< "$files"
  echo "$s"
}

case "$MODE" in
  full) STEPS=" $ALL " ;;
  quick) STEPS=" $QUICK " ;;
  changed)
    STEPS=$(select_changed)
    if [ -z "${STEPS// /}" ]; then
      echo "=== nothing to check: no changes since $BASE (use --quick or --full to force)"
      exit 0
    fi ;;
esac

RESULTS=()
FAILED=0
LOG_DIR="$ROOT/target/ci-logs"
mkdir -p "$LOG_DIR"

have() { command -v "$1" >/dev/null 2>&1; }
wanted() {
  [[ "$STEPS" == *" $1 "* ]] || return 1
  [ -n "$ONLY" ] && [[ "$ONLY" != *",$1,"* ]] && return 1
  [[ "$SKIP" == *",$1,"* ]] && return 1
  return 0
}
if [ "$LIST" = 1 ]; then
  for s in $ALL; do wanted "$s" && echo "$s"; done
  exit 0
fi
skip() { RESULTS+=("SKIP  $1  ($2)"); echo "--- SKIP $1: $2"; }
# step NAME COMMAND...: run COMMAND through bash, log to target/ci-logs/NAME.log.
step() {
  local name="$1"; shift
  wanted "$name" || return 0
  echo "=== $name"
  local t0=$SECONDS
  if bash -o pipefail -c "$*" > >(tee "$LOG_DIR/$name.log") 2>&1; then
    RESULTS+=("PASS  $name  ($((SECONDS - t0)) s)")
  else
    RESULTS+=("FAIL  $name  ($((SECONDS - t0)) s, log: target/ci-logs/$name.log)")
    FAILED=1
  fi
}

OS="$(uname -s)"
T_START=$SECONDS
echo "=== mode: $MODE$([ "$MODE" = changed ] && echo " (since $BASE)"); steps:$STEPS"

# --- Rust: format, lints, tests ----------------------------------------------------------
step fmt "cargo fmt --all --check"
step clippy "cargo clippy --workspace --all-targets -- -D warnings && cargo clippy -p flashraw --all-features -- -D warnings"
step test-debug "cargo test --lib -p flashraw-core -p flashraw-codecs -p flashraw-formats -p flashraw"
# The C/C++ example tests link the cdylib/staticlib files, which `cargo test` does not
# rebuild: build them first so the examples never pick up stale libraries.
step test-release "cargo build --release -p flashraw-capi -p flashraw-libraw-compat && cargo test --workspace --release"
step no-default-features "cargo test -p flashraw --no-default-features --release --lib"
if wanted corpus; then
  if [ -d "$FLASHRAW_CORPUS" ]; then
    step corpus "cargo test --release -p flashraw --test regression -- --nocapture && cargo test --release -p flashraw --test malformed"
  else
    skip corpus "no corpus at $FLASHRAW_CORPUS (tools/corpus/fetch.py)"
  fi
fi

# --- Minimum supported Rust versions ----------------------------------------------------
if wanted msrv; then
  if have rustup; then
    step msrv "rustup toolchain install 1.85 --profile minimal >/dev/null && rustup toolchain install 1.88 --profile minimal >/dev/null \
      && cargo +1.85 check -p flashraw --features serde && cargo +1.88 check --workspace --all-targets --all-features"
  else
    skip msrv "rustup not found"
  fi
fi

# --- Camera database, generated header, GPU build ---------------------------------------
step camera-db "cargo run -q --release -p flashraw-cli -- camera-db validate camera-db/cameras.json && python3 tools/support_matrix.py --check"
if wanted c-header; then
  if have cbindgen; then
    step c-header "cbindgen --quiet --config crates/flashraw-capi/cbindgen.toml --crate flashraw-capi --output target/ci-flashraw.h \
      && diff -u crates/flashraw-capi/include/flashraw.h target/ci-flashraw.h"
  else
    skip c-header "cbindgen not found (cargo install cbindgen --locked --version 0.29.4)"
  fi
fi
step gpu "cargo build -p flashraw-cli --features gpu --release && cargo test -p flashraw-gpu --release"

# --- C API: CMake install, examples against the install, pkg-config ---------------------
if wanted c-api || wanted c-api-pkgconfig; then
  if have "$CMAKE"; then
    B="$ROOT/target/ci-capi"
    step c-api "rm -rf '$B' && '$CMAKE' -S crates/flashraw-capi -B '$B/capi' -DCMAKE_BUILD_TYPE=Release -DFLASHRAW_CARGO_TARGET_DIR='$ROOT/target' \
      && '$CMAKE' --build '$B/capi' --config Release && '$CMAKE' --install '$B/capi' --prefix '$B/prefix' --config Release \
      && '$CMAKE' -S examples -B '$B/examples' -DCMAKE_PREFIX_PATH='$B/prefix' -DCMAKE_BUILD_TYPE=Release \
      && '$CMAKE' --build '$B/examples' --config Release && '$CTEST' --test-dir '$B/examples' -C Release --output-on-failure"
    if have pkg-config; then
      step c-api-pkgconfig "export PKG_CONFIG_PATH=\$(dirname \$(find '$B/prefix' -name flashraw.pc)) \
        && cc -std=c99 examples/c/flashraw_info.c \$(pkg-config --cflags --libs flashraw) -Wl,-rpath,\$(pkg-config --variable=libdir flashraw) -o '$B/info' \
        && '$B/info' npm/tests/fixtures/canon-5d3-mlv.dng"
    else
      # Same check without pkg-config: resolve the .pc file's own variables.
      step c-api-pkgconfig "pc=\$(find '$B/prefix' -name flashraw.pc) && d=\$(dirname \$pc) \
        && pre=\$(sed -n 's/^prefix=\${pcfiledir}\///p' \$pc) && inc=\$d/\$pre/\$(sed -n 's/^includedir=\${prefix}\///p' \$pc) \
        && lib=\$d/\$pre/\$(sed -n 's/^libdir=\${prefix}\///p' \$pc) \
        && cc -std=c99 examples/c/flashraw_info.c -I\$inc -L\$lib -lflashraw_capi -Wl,-rpath,\$lib -o '$B/info' \
        && '$B/info' npm/tests/fixtures/canon-5d3-mlv.dng"
    fi
  else
    wanted c-api && skip c-api "cmake not found (set CMAKE=/path/to/cmake)"
  fi
fi

# --- WASM and the npm package -----------------------------------------------------------
HAVE_WASM32=0
have rustup && rustup target list --installed 2>/dev/null | grep -q wasm32-unknown-unknown && HAVE_WASM32=1
if wanted wasm; then
  if [ "$HAVE_WASM32" = 1 ]; then
    step wasm "cargo test -p flashraw-wasm && cargo build -p flashraw-wasm --target wasm32-unknown-unknown --release \
      && RUSTFLAGS='-C target-feature=+simd128' cargo build -p flashraw-wasm --target wasm32-unknown-unknown --release \
      && cargo check -p flashraw-gpu --target wasm32-unknown-unknown"
  else
    skip wasm "wasm32-unknown-unknown target not installed"
  fi
fi
if wanted npm || wanted npm-browser || wanted npm-bundlers; then
  if [ "$HAVE_WASM32" = 0 ]; then
    skip npm "wasm32-unknown-unknown target not installed (rustup target add wasm32-unknown-unknown)"
  elif have npm && have node; then
    step npm "cargo build --release -p flashraw-cli --features jpegxl && cd npm && npm ci --no-audit --no-fund && npm run build \
      && npm test && FLASHRAW_WASM_THREADS=0 npm test && npm run test:types && npm pack --dry-run >/dev/null"
    step npm-browser "cd npm && npm run test:browser"
    step npm-bundlers "cd npm && npm run test:bundlers"
  else
    skip npm "node/npm not found"
  fi
fi

# --- Python bindings --------------------------------------------------------------------
if wanted python; then
  if have python3; then
    VENV="${PYTHON_VENV:-$ROOT/target/ci-venv}"
    # Some system Pythons ship without ensurepip: create the venv without pip and
    # bootstrap it.
    step python "if [ ! -x '$VENV/bin/pip' ]; then rm -rf '$VENV'; python3 -m venv '$VENV' 2>/dev/null \
        || { python3 -m venv --without-pip '$VENV' && curl -fsSL https://bootstrap.pypa.io/get-pip.py | '$VENV/bin/python' - -q; }; fi; \
      '$VENV/bin/pip' install -q --upgrade pip maturin numpy pytest pillow mypy \
      && cd crates/flashraw-python && VIRTUAL_ENV='$VENV' PATH='$VENV/bin':\$PATH maturin develop --release -q \
      && '$VENV/bin/pytest' -q tests && '$VENV/bin/mypy' --strict python/flashraw"
  else
    skip python "python3 not found"
  fi
fi

# --- Fuzz smoke, benches ----------------------------------------------------------------
if wanted fuzz; then
  if have cargo-fuzz && rustup toolchain list 2>/dev/null | grep -q nightly; then
    step fuzz "cd fuzz && cargo +nightly fuzz build -O && for t in \$(cargo +nightly fuzz list); do \
      if [ -d regressions/\$t ]; then ./target/*/release/\$t regressions/\$t/* || exit 1; fi; \
      cargo +nightly fuzz run -O \$t -- -max_total_time=30 -rss_limit_mb=2048 || exit 1; done"
  else
    skip fuzz "cargo-fuzz or the nightly toolchain not found"
  fi
fi
step benches "cargo bench --workspace --no-run"

# --- Summary ----------------------------------------------------------------------------
echo
echo "=== local CI summary ($OS, $(git rev-parse --short HEAD 2>/dev/null || echo '?'), mode $MODE, $((SECONDS - T_START)) s)"
printf '%s\n' "${RESULTS[@]}"
exit $FAILED
