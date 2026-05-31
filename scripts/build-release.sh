#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

VERSION="${VERSION:-$(cargo metadata --no-deps --format-version 1 | sed -n 's/.*"version":"\([^"]*\)".*/\1/p' | head -n1)}"
TARGET_TRIPLE="${TARGET:-$(rustc -vV | sed -n 's/^host: //p')}"
BIN_NAME="fast-context-rust"
EXE_SUFFIX=""
if [[ "$TARGET_TRIPLE" == *"windows"* ]]; then
  EXE_SUFFIX=".exe"
fi

BUILD_ARGS=(build --release)
if [[ -n "${TARGET:-}" ]]; then
  BUILD_ARGS+=(--target "$TARGET")
fi
cargo "${BUILD_ARGS[@]}"

BIN_PATH="target/release/${BIN_NAME}${EXE_SUFFIX}"
if [[ -n "${TARGET:-}" ]]; then
  BIN_PATH="target/${TARGET}/release/${BIN_NAME}${EXE_SUFFIX}"
fi

if [[ ! -x "$BIN_PATH" ]]; then
  echo "release binary not found or not executable: $BIN_PATH" >&2
  exit 1
fi

DIST_DIR="dist"
PKG_DIR="${DIST_DIR}/${BIN_NAME}-${VERSION}-${TARGET_TRIPLE}"
rm -rf "$PKG_DIR"
mkdir -p "$PKG_DIR"
cp "$BIN_PATH" "$PKG_DIR/"
cp README.md README_CN.md "$PKG_DIR/"

(
  cd "$DIST_DIR"
  ARCHIVE="${BIN_NAME}-${VERSION}-${TARGET_TRIPLE}.tar.gz"
  rm -f "$ARCHIVE" "$ARCHIVE.sha256"
  tar -czf "$ARCHIVE" "$(basename "$PKG_DIR")"
  sha256sum "$ARCHIVE" > "$ARCHIVE.sha256"
  echo "Created $DIST_DIR/$ARCHIVE"
  echo "Created $DIST_DIR/$ARCHIVE.sha256"
)
