#!/usr/bin/env bash
# nels-core purity gate (spec savvagent/nels-oss#5, A14). Run from anywhere.
# 1. The Wasm dependency tree (default features, i.e. no `sqlx`) must not
#    contain an I/O or entropy crate.
# 2. Non-test source must not reach the environment, filesystem, network,
#    processes or the clock. Each file is scanned only up to its first
#    `#[cfg(test)]` line: the test module is always the LAST item in a core
#    file, so everything after that line is test code. Comment lines are
#    skipped so docs may name what the crate avoids.
# 3. "The test module is the file's only `#[cfg(test)]` and its LAST item"
#    is ENFORCED here, not just assumed: a file fails if it has more than
#    one `#[cfg(test)]` line, or if any non-blank, non-comment line follows
#    the closing brace of the `mod tests { ... }` block that follows its
#    (single) `#[cfg(test)]`. The closing brace is found by brace-depth
#    counting from the `mod tests {` line onward.
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

  cfg_count=$(grep -c '#\[cfg(test)\]' "$f" || true)
  cfg_count=${cfg_count:-0}
  if [ "$cfg_count" -gt 1 ]; then
    echo "::error::$f:1: $cfg_count occurrences of '#[cfg(test)]'; the test module must be the file's only one, and its LAST item."
    fail=1
  fi

  if [ "$cfg_count" -eq 1 ]; then
    trailing=$(awk '
      state == 0 && /#\[cfg\(test\)\]/ { state = 1; next }
      state == 1 {
        if (match($0, /mod[ \t]+tests[ \t]*\{/)) {
          state = 2
          line = $0
          n = gsub(/\{/, "{", line)
          m = gsub(/\}/, "}", line)
          depth = n - m
          if (depth <= 0) { state = 3 }
        }
        next
      }
      state == 2 {
        line = $0
        n = gsub(/\{/, "{", line)
        m = gsub(/\}/, "}", line)
        depth += n - m
        if (depth <= 0) { state = 3 }
        next
      }
      state == 3 {
        stripped = $0
        gsub(/^[ \t]+/, "", stripped)
        if (stripped != "" && substr(stripped, 1, 2) != "//") {
          print FILENAME ":" NR ": " $0
        }
      }
    ' "$f" || true)
    if [ -n "$trailing" ]; then
      echo "::error::non-blank, non-comment code follows the closing brace of mod tests (it must be the file's last item):"
      echo "$trailing"
      fail=1
    fi
  fi
done < <(find src -name '*.rs' | sort)

if [ "$fail" -ne 0 ]; then exit 1; fi
echo "purity: ok"
