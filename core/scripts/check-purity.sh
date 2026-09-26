#!/usr/bin/env bash
# nels-core purity gate (spec savvagent/nels-oss#5, A14). Run from anywhere.
# 1. The Wasm dependency tree (default features, i.e. no `sqlx`) must not
#    contain an I/O or entropy crate.
# 2. Non-test source must not reach the environment, filesystem, network,
#    processes or the clock. Each file is scanned only up to its first
#    `#[cfg(test)]` line: the test module is always the LAST item in a core
#    file, so everything after that line is test code. Comment lines are
#    skipped so docs may name what the crate avoids.
set -euo pipefail
cd "$(dirname "$0")/.."

tree=$(cargo tree --target wasm32-unknown-unknown -e normal --prefix none)
if [ -z "$tree" ]; then
  echo "::error::cargo tree printed nothing; refusing to read that as a clean tree."
  exit 1
fi
if hits=$(grep -E '^(getrandom|tokio|sqlx|sqlx-core|reqwest|axum|hyper|mio|wasm-bindgen|js-sys) ' <<<"$tree"); then
  echo "::error::forbidden crates in the Wasm dependency tree:"
  echo "$hits"
  exit 1
fi

fail=0
while IFS= read -r f; do
  hits=$(awk '/#\[cfg\(test\)\]/ { exit } { print FILENAME ":" NR ": " $0 }' "$f" \
    | grep -vE '^[^:]+:[0-9]+: *//' \
    | grep -E 'std::(env|fs|net|process)|SystemTime|Instant|Utc::now|Local::now' || true)
  if [ -n "$hits" ]; then
    echo "::error::I/O or clock use in non-test code:"
    echo "$hits"
    fail=1
  fi
done < <(find src -name '*.rs' | sort)

if [ "$fail" -ne 0 ]; then exit 1; fi
echo "purity: ok"
