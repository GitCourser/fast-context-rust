#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

cargo build --quiet
BIN="target/debug/fast-context-rust"

"$BIN" --check-rg >/tmp/fast-context-rust-rg-version.txt
if ! grep -q '^ripgrep ' /tmp/fast-context-rust-rg-version.txt; then
  echo "--check-rg did not print ripgrep version" >&2
  cat /tmp/fast-context-rust-rg-version.txt >&2
  exit 1
fi

TOOLS_OUTPUT="$({ printf '%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'; } | "$BIN")"

echo "$TOOLS_OUTPUT" | grep -q '"name":"fast-context-rust"'
echo "$TOOLS_OUTPUT" | grep -q '"name":"fast_context_search"'
if echo "$TOOLS_OUTPUT" | grep -q 'extract_windsurf_key'; then
  echo "extract_windsurf_key must not be exposed as an MCP tool" >&2
  exit 1
fi

set +e
FC_RG_PATH="definitely-missing-rg-for-fast-context-rust-smoke" "$BIN" >/tmp/fast-context-rust-missing-rg.out 2>/tmp/fast-context-rust-missing-rg.err
STATUS=$?
set -e
if [[ "$STATUS" -eq 0 ]]; then
  echo "missing rg preflight unexpectedly succeeded" >&2
  exit 1
fi
grep -q 'Install examples' /tmp/fast-context-rust-missing-rg.err

echo "fast-context-rust smoke test passed"
