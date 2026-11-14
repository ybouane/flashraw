#!/usr/bin/env bash
# Licence check: every package carries byte-identical copies of the root LICENSE-MIT and
# LICENSE-APACHE, and every publishable manifest says `MIT OR Apache-2.0`.
#
#   tools/ci/check-licenses.sh
#
# Packages checked: the crates under crates/ (copies required for the ones that are published or
# shipped as release artifacts; see COPIES), npm/, and the Python package (crates/flashraw-python).
set -euo pipefail
cd "$(dirname "$0")/../.."

SPDX='MIT OR Apache-2.0'
# Directories that must hold copies of both licence files.
COPIES="crates/flashraw-core crates/flashraw-codecs crates/flashraw-formats crates/flashraw-camera-db
crates/flashraw-gpu crates/flashraw crates/flashraw-cli crates/flashraw-capi crates/flashraw-python npm"

fail=0
err() { echo "licenses: $*" >&2; fail=1; }

for d in $COPIES; do
  for f in LICENSE-MIT LICENSE-APACHE; do
    if [ ! -f "$d/$f" ] || [ -L "$d/$f" ]; then err "$d/$f is missing or a symlink"
    elif ! cmp -s "$f" "$d/$f"; then err "$d/$f differs from the root $f"; fi
  done
done

# Cargo manifests: workspace.package.license, and each crate either inherits it or states it.
grep -q "^license = \"$SPDX\"" Cargo.toml || err "Cargo.toml: workspace license is not \"$SPDX\""
for m in crates/*/Cargo.toml; do
  if grep -q '^license.workspace = true' "$m"; then continue; fi
  grep -q "^license = \"$SPDX\"" "$m" || err "$m: license is not \"$SPDX\""
done

# npm and Python.
grep -q "\"license\": \"$SPDX\"" npm/package.json || err "npm/package.json: license is not \"$SPDX\""
for f in LICENSE-MIT LICENSE-APACHE; do
  grep -q "\"$f\"" npm/package.json || err "npm/package.json: \"files\" does not list $f"
  grep -q "\"$f\"" crates/flashraw-python/pyproject.toml || err "pyproject.toml: license-files does not list $f"
done
grep -q "^license = \"$SPDX\"" crates/flashraw-python/pyproject.toml || err "pyproject.toml: license is not \"$SPDX\""

# Every published crate lists the files in its package.
if command -v cargo >/dev/null 2>&1 && [ "${FLASHRAW_LICENSES_PACKAGE_LIST:-0}" = 1 ]; then
  for c in flashraw-core flashraw-codecs flashraw-formats flashraw-camera-db flashraw-gpu flashraw flashraw-cli; do
    l=$(cargo package -p "$c" --list --allow-dirty 2>/dev/null)
    for f in LICENSE-MIT LICENSE-APACHE; do
      grep -qx "$f" <<<"$l" || err "cargo package --list of $c does not contain $f"
    done
  done
fi

[ "$fail" = 0 ] && echo "licenses: ok"
exit "$fail"
