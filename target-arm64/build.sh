#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 The TotalDrop Contributors
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Apple Silicon (M1-M4, aarch64-apple-darwin) build unit for TotalDrop on macOS.
# Produces TotalDrop.app containing TotalDropMac and FinderShareExtension.appex.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
RUST_TARGET="aarch64-apple-darwin"
SWIFT_TRIPLE="arm64-apple-macosx15.0"
VERSION="${TOTALDROP_VERSION:-0.2.0-beta.5}"
OUT_DIR="${ROOT_DIR}/dist/arm64"
APP_BUNDLE="${OUT_DIR}/TotalDrop.app"

export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-stable}"
export MACOSX_DEPLOYMENT_TARGET="15.0"

echo "==> [arm64] Building Rust static bridge (${RUST_TARGET})"
rustup target add "${RUST_TARGET}"
cargo build \
  --manifest-path "${ROOT_DIR}/Cargo.toml" \
  --package totaldrop-macos-bridge \
  --target "${RUST_TARGET}" \
  --release \
  --locked

echo "==> [arm64] Building Swift 6 macOS UI and Finder Share Extension (${SWIFT_TRIPLE})"
swift build \
  --package-path "${ROOT_DIR}/ui-swiftui-macos" \
  --triple "${SWIFT_TRIPLE}" \
  --configuration release \
  -Xlinker "-L${ROOT_DIR}/target/${RUST_TARGET}/release" \
  -Xlinker "-ltotaldrop_macos_bridge"

echo "==> [arm64] Assembling TotalDrop.app bundle"
rm -rf "${APP_BUNDLE}"
mkdir -p \
  "${APP_BUNDLE}/Contents/MacOS" \
  "${APP_BUNDLE}/Contents/PlugIns/FinderShareExtension.appex/Contents/MacOS" \
  "${APP_BUNDLE}/Contents/Resources"

cp "${ROOT_DIR}/ui-swiftui-macos/.build/${SWIFT_TRIPLE}/release/TotalDropMac" \
   "${APP_BUNDLE}/Contents/MacOS/TotalDropMac"
sed "s/@VERSION@/${VERSION}/g" "${SCRIPT_DIR}/Info.plist" > "${APP_BUNDLE}/Contents/Info.plist"
cp "${ROOT_DIR}/ui-swiftui-macos/apple-app-site-association" \
   "${APP_BUNDLE}/Contents/Resources/apple-app-site-association"

echo "APPL????" > "${APP_BUNDLE}/Contents/PkgInfo"
echo "==> [arm64] Bundle staged at ${APP_BUNDLE}"
