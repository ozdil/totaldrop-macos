# TotalDrop — Native macOS Module (`07-macos`)

Native macOS desktop application for [TotalDrop](https://totaldrop.app), built with **Swift 6 strict concurrency** (`SwiftUI` + `AppKit`) over the shared Rust client engine (`totaldrop-core`, `totaldrop-proto`, `totaldrop-crypto`, and `totaldrop-l10n`) in accordance with **ADR 0009** and **ADR 0019**.

## Architecture

| Layer | Path | Responsibility |
| --- | --- | --- |
| Protocol, Crypto & Core | `crates/totaldrop-{proto,crypto,core,l10n}` | CPace PAKE, QUIC + Noise `XXpsk3` + ML-KEM-768 hybrid transport, STREAM chunk AEAD, BLAKE3 file verification, and 35-language ICU localization |
| Safe macOS Rust Bridge | `crates/totaldrop-macos-bridge` | `#![forbid(unsafe_code)]` bridge for `totaldrop://` & Universal Links (`https://totaldrop.app/r#<code>`), Finder `file://` APFS NFD-to-NFC validation, 35-language `Locale.preferredLanguages` negotiation, RTL direction, and regional progress formatting |
| Native Swift 6 UI | `ui-swiftui-macos/` | Swift 6 `TotalDropMac` (`NavigationSplitView` vibrancy materials, `MenuBarExtra`, `IOKit` sleep assertion, `Vision` QR reader, `CoreImage` QR generator) and `FinderShareExtension` (`NSViewController` Share / Quick Actions) |
| Apple Silicon Unit | `target-arm64/` | Isolated M1–M4 (`aarch64-apple-darwin`) build, ADR 0009 ad-hoc / Developer ID notarization, UDZO `.dmg` packaging, and Homebrew Cask recipe |
| Intel Mac Unit | `target-x86_64/` | Isolated Intel (`x86_64-apple-darwin`) build, codesigning/notarization, `.dmg` recipe, and `lipo` universal binary merge |

## Deep Links & Universal Links

- **Universal Links:** `ui-swiftui-macos/apple-app-site-association` binds `https://totaldrop.app/r#<code>` (`applinks:totaldrop.app`) so the transfer code remains in the client-only URL fragment and is never transmitted in an HTTP request path.
- **Custom URL Scheme:** `totaldrop://r/<code>` registered in `target-arm64/Info.plist` (`CFBundleURLTypes`).
- Both routes funnel through `TransferCode::from_link` in `totaldrop-macos-bridge`, stripping invisible bidi marks, percent-decoding once, and guarding active transfers (`linkBusy`).

## Verification

Run the full Rust workspace lint and test suite:

```bash
RUSTUP_TOOLCHAIN=stable cargo clippy --workspace --all-targets -- -D warnings
RUSTUP_TOOLCHAIN=stable cargo test --workspace
```

On macOS 15+ (pinned GitHub `macos-15` runner per ADR 0009):

```bash
./target-arm64/build.sh
./target-arm64/codesign-and-notarize.sh
./target-arm64/create-dmg.sh
```

Verify artifact provenance before opening an unnotarized CI build:

```bash
shasum -a 256 -c dist/arm64/SHA256SUMS
gh attestation verify dist/arm64/totaldrop-macos-0.2.0-beta.5-arm64.dmg --repo totaldrop-app/totaldrop
```

## License

- Application and macOS bridge (`crates/totaldrop-macos-bridge`, `ui-swiftui-macos`, `target-*`): `GPL-3.0-or-later`
- Core protocol and localization libraries (`crates/totaldrop-{core,crypto,proto,l10n}`): `MIT OR Apache-2.0`
