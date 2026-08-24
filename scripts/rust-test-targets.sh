#!/usr/bin/env bash
#
# Prints the Rust targets belonging to one reusable test battery.
#
# `core` owns ordinary integration tests, both workspace library targets, and
# (through run-tests.sh) doctests. `contracts` owns integration tests that
# inspect tracked checkout files at runtime. Keeping those checkout readers in
# their own battery lets a browser/spec/doc edit rerun the contracts it can
# affect without throwing away thousands of unrelated Rust results.

set -euo pipefail

mode="${1:-}"
case "$mode" in
(core | contracts) ;;
(*)
    echo "rust-test-targets: expected core or contracts" >&2
    exit 1
    ;;
esac

cd "$(dirname "$0")/.."

cargo metadata --no-deps --format-version=1 | python3 -c '
import json
import pathlib
import sys

mode = sys.argv[1]
metadata = json.load(sys.stdin)

integration = []
libraries = []
for package in metadata["packages"]:
    for target in package["targets"]:
        kinds = set(target["kind"])
        if "lib" in kinds:
            libraries.append(target["name"])
        if "test" not in kinds:
            continue
        source = pathlib.Path(target["src_path"]).read_text(encoding="utf-8")
        checkout_reader = any(
            marker in source
            for marker in ("CARGO_MANIFEST_DIR", "git ls-files", "include_str!")
        )
        integration.append((target["name"], checkout_reader))

if mode == "core":
    names = [name for name, is_contract in integration if not is_contract]
    names.extend(libraries)
else:
    names = [name for name, is_contract in integration if is_contract]

for name in sorted(set(names)):
    print(name)
' "$mode"
