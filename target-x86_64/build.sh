#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 The TotalDrop Contributors
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Intel Mac (x86_64-apple-darwin) build unit for TotalDrop on macOS, plus
# optional `lipo` universal binary assembly when paired with target-arm64.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
RUST_TARGET="x86_64-apple-darwin"
SWIFT_TRIPLE="x86_64-apple-macosx15.0"
VERSION="${TOTALDROP_VERSION:-0.2.0-beta.5}"
OUT_DIR="${ROOT_DIR}/dist/x86_64"
APP_BUNDLE="${OUT_DIR}/TotalDrop.app"

export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-stable}"
export MACOSX_DEPLOYMENT_TARGET="15.0"

echo "==> [x86_64] Building Rust static bridge (${RUST_TARGET})"
rustup target add "${RUST_TARGET}"
cargo build \
  --manifest-path "${ROOT_DIR}/Cargo.toml" \
  --package totaldrop-macos-bridge \
  --target "${RUST_TARGET}" \
  --release \
  --locked

echo "==> [x86_64] Building Swift 6 macOS UI and Finder Share Extension (${SWIFT_TRIPLE})"
swift build \
  --package-path "${ROOT_DIR}/ui-swiftui-macos" \
  --triple "${SWIFT_TRIPLE}" \
  --configuration release \
  -Xlinker "-L${ROOT_DIR}/target/${RUST_TARGET}/release" \
  -Xlinker "-ltotaldrop_macos_bridge"

echo "==> [x86_64] Assembling TotalDrop.app bundle"
rm -rf "${APP_BUNDLE}"
mkdir -p \
  "${APP_BUNDLE}/Contents/MacOS" \
  "${APP_BUNDLE}/Contents/PlugIns/FinderShareExtension.appex/Contents/MacOS" \
  "${APP_BUNDLE}/Contents/Resources"

cp "${ROOT_DIR}/ui-swiftui-macos/.build/${SWIFT_TRIPLE}/release/TotalDropMac" \
   "${APP_BUNDLE}/Contents/MacOS/TotalDropMac"
sed "s/@VERSION@/${VERSION}/g" "${ROOT_DIR}/target-arm64/Info.plist" > "${APP_BUNDLE}/Contents/Info.plist"
cp "${ROOT_DIR}/ui-swiftui-macos/apple-app-site-association" \
   "${APP_BUNDLE}/Contents/Resources/apple-app-site-association"

if [[ "${BUILD_UNIVERSAL:-0}" == "1" && -f "${ROOT_DIR}/dist/arm64/TotalDrop.app/Contents/MacOS/TotalDropMac" ]]; then
  echo "==> [universal] Merging arm64 + x86_64 executables via lipo (ADR 0009)"
  mkdir -p "${ROOT_DIR}/dist/universal"
  cp -R "${APP_BUNDLE}" "${ROOT_DIR}/dist/universal/TotalDrop.app"
  lipo -create \
    -output "${ROOT_DIR}/dist/universal/TotalDrop.app/Contents/MacOS/TotalDropMac" \
    "${ROOT_DIR}/dist/arm64/TotalDrop.app/Contents/MacOS/TotalDropMac" \
    "${APP_BUNDLE}/Contents/MacOS/TotalDropMac"
fi

echo "APPL????" > "${APP_BUNDLE}/Contents/PkgInfo"
echo "==> [x86_64] Bundle staged at ${APP_BUNDLE}"
