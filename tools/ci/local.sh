#!/usr/bin/env bash
# Local CI: the checks of .github/workflows/{ci,wasm,python}.yml, run on this machine.
#
#   tools/ci/local.sh [--quick] [--only step,step] [--skip step,step]
#
# Steps run in order and the script continues past failures; a summary is printed at the
# end and the exit status is non-zero if any step failed. A step whose tools are missing is
# reported as SKIP (never as a pass). Environment:
#   FLASHRAW_CORPUS  corpus root for the regression/malformed suites (default: ./corpus)
#   CMAKE, CTEST     CMake binaries (default: cmake / ctest on PATH)
#   PYTHON_VENV      virtualenv for the Python bindings (default: target/ci-venv, created)
#   CARGO_TARGET_DIR is honoured as usual.
# --quick skips the slow steps (msrv, fuzz, browser and bundler tests, benches).

set -uo pipefail
cd "$(dirname "$0")/../.."
ROOT="$PWD"
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}"
export FLASHRAW_CORPUS="${FLASHRAW_CORPUS:-$ROOT/corpus}"
CMAKE="${CMAKE:-cmake}"
CTEST="${CTEST:-ctest}"
QUICK=0
ONLY=""
SKIP=""
while [ $# -gt 0 ]; do
  case "$1" in
    --quick) QUICK=1 ;;
    --only) ONLY=",$2,"; shift ;;
    --skip) SKIP=",$2,"; shift ;;
    -h|--help) sed -n '2,15p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

RESULTS=()
FAILED=0
LOG_DIR="$ROOT/target/ci-logs"
mkdir -p "$LOG_DIR"

have() { command -v "$1" >/dev/null 2>&1; }
wanted() {
  [ -n "$ONLY" ] && [[ "$ONLY" != *",$1,"* ]] && return 1
  [[ "$SKIP" == *",$1,"* ]] && return 1
  return 0
}
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

# --- Rust: format, lints, tests ----------------------------------------------------------
step fmt "cargo fmt --all --check"
step clippy "cargo clippy --workspace --all-targets -- -D warnings && cargo clippy -p flashraw --all-features -- -D warnings"
step test-debug "cargo test --lib -p flashraw-core -p flashraw-codecs -p flashraw-formats -p flashraw"
step test-release "cargo test --workspace --release"
step no-default-features "cargo test -p flashraw --no-default-features --release --lib"
if [ -d "$FLASHRAW_CORPUS" ]; then
  step corpus "cargo test --release -p flashraw --test regression -- --nocapture && cargo test --release -p flashraw --test malformed"
else
  wanted corpus && skip corpus "no corpus at $FLASHRAW_CORPUS (tools/corpus/fetch.py)"
fi

# --- Minimum supported Rust versions ----------------------------------------------------
if [ "$QUICK" = 0 ] && wanted msrv; then
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
if wanted c-api; then
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
    skip c-api "cmake not found (set CMAKE=/path/to/cmake)"
  fi
fi

# --- WASM and the npm package -----------------------------------------------------------
if wanted wasm; then
  if have rustup && rustup target list --installed 2>/dev/null | grep -q wasm32-unknown-unknown; then
    step wasm "cargo test -p flashraw-wasm && cargo build -p flashraw-wasm --target wasm32-unknown-unknown --release \
      && RUSTFLAGS='-C target-feature=+simd128' cargo build -p flashraw-wasm --target wasm32-unknown-unknown --release \
      && cargo check -p flashraw-gpu --target wasm32-unknown-unknown"
  else
    skip wasm "wasm32-unknown-unknown target not installed"
  fi
fi
HAVE_WASM32=0
have rustup && rustup target list --installed 2>/dev/null | grep -q wasm32-unknown-unknown && HAVE_WASM32=1
if wanted npm; then
  if [ "$HAVE_WASM32" = 0 ]; then
    skip npm "wasm32-unknown-unknown target not installed (rustup target add wasm32-unknown-unknown)"
  elif have npm && have node; then
    step npm "cargo build --release -p flashraw-cli --features jpegxl && cd npm && npm ci --no-audit --no-fund && npm run build \
      && npm test && FLASHRAW_WASM_THREADS=0 npm test && npm run test:types && npm pack --dry-run >/dev/null"
    if [ "$QUICK" = 0 ]; then
      step npm-browser "cd npm && npm run test:browser"
      step npm-bundlers "cd npm && npm run test:bundlers"
    fi
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
if [ "$QUICK" = 0 ] && wanted fuzz; then
  if have cargo-fuzz && rustup toolchain list 2>/dev/null | grep -q nightly; then
    step fuzz "cd fuzz && cargo +nightly fuzz build -O && for t in \$(cargo +nightly fuzz list); do \
      if [ -d regressions/\$t ]; then ./target/*/release/\$t regressions/\$t/* || exit 1; fi; \
      cargo +nightly fuzz run -O \$t -- -max_total_time=30 -rss_limit_mb=2048 || exit 1; done"
  else
    skip fuzz "cargo-fuzz or the nightly toolchain not found"
  fi
fi
[ "$QUICK" = 0 ] && step benches "cargo bench --workspace --no-run"

# --- Summary ----------------------------------------------------------------------------
echo
echo "=== local CI summary ($OS, $(git rev-parse --short HEAD 2>/dev/null || echo '?'))"
printf '%s\n' "${RESULTS[@]}"
exit $FAILED
