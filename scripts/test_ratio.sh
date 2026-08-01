#!/usr/bin/env bash
# Test:code ratio gate (§15 DoD). Exits 0 when test LOC ≥ implementation LOC
# across both crates.
#
# LOC = code lines only (blanks + full-line comments excluded, standard SLOC
# definition à la tokei/cloc).
#   implementation LOC = src/ minus #[cfg(test)] modules
#   test LOC = #[cfg(test)] modules + tests/
set -euo pipefail
cd "$(dirname "$0")/.."

code_lines() {
  # stdin → count of non-blank, non-comment code lines
  awk '
    /^[[:space:]]*$/ {next}                 # blank
    /^[[:space:]]*\/\// {next}              # // comment
    /^[[:space:]]*\/\*/ {in_block=1; next}  # /* comment start
    in_block && /\*\// {in_block=0; next}
    in_block {next}
    {n++}
    END {print n+0}
  '
}

impl_loc=0
test_loc=0

for crate in crates/core crates/bot; do
  src="$crate/src"
  [ -d "$src" ] || continue

  # Implementation: code lines in src/ minus #[cfg(test)] blocks.
  impl_loc=$((impl_loc + $(find "$src" -name '*.rs' -exec cat {} + | awk '
    /^#\[cfg\(test\)\]/ {in_test=1; next}
    in_test && /^}/ {in_test=0; next}
    !in_test {print}
  ' | code_lines)))

  # Test LOC: #[cfg(test)] blocks in src/ + tests/ dir.
  test_loc=$((test_loc + $(find "$src" -name '*.rs' -exec cat {} + | awk '
    /^#\[cfg\(test\)\]/ {in_test=1}
    in_test {print}
    in_test && /^}/ {in_test=0}
  ' | code_lines)))
  if [ -d "$crate/tests" ]; then
    test_loc=$((test_loc + $(find "$crate/tests" -name '*.rs' -exec cat {} + | code_lines)))
  fi
done

echo "impl LOC: $impl_loc | test LOC: $test_loc"
if [ "$test_loc" -ge "$impl_loc" ]; then
  echo "PASS: test:code ratio >= 1:1"
  exit 0
else
  echo "FAIL: test:code ratio < 1:1 (need $((impl_loc - test_loc)) more test LOC)"
  exit 1
fi
