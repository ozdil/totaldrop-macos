// SPDX-FileCopyrightText: 2026 The TotalDrop Contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//
//! Safe Rust bridge for the native macOS Swift 6 / SwiftUI application (ADR 0009).
//!
//! This crate connects `TotalDropMac` and its `FinderShareExtension` to
//! [`totaldrop_core`], [`totaldrop_proto`], and [`totaldrop_l10n`] without any
//! `unsafe` code:
//! - `totaldrop://` custom URL scheme and `https://totaldrop.app/r#<code>`
//!   Universal Links parsing and busy-window routing;
//! - Finder Quick Actions, `NSSharingService`, and drag-and-drop `file://`
//!   batch validation with APFS NFD-to-NFC path normalization and case-collision
//!   detection;
//! - 35-language macOS localization negotiation, RTL direction, safety-critical
//!   draft guards, and hidden control-character revelation;
//! - regional byte-size, percentage, and `MenuBarExtra` progress formatting;
//! - transfer configuration, cancellation (`Stop`), and localized error mapping.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::json;
use totaldrop_core::{
    Error as CoreError, IncomingOffer, PathOptions, PreparedSend, ReceiveOptions, SendOptions, Stop,
};
use totaldrop_l10n::{Arg, Domain, Strings};
use totaldrop_proto::{SafePath, TransferCode, limits, validate_path};

pub mod coordinator;

pub use coordinator::{
    MAX_OFFER_PREVIEW_NAMES, MacOfferPreview, MacSessionCoordinator, MacSessionPhase,
    MacSessionRole, MacSessionSnapshot,
};

/// Maximum number of characters rendered inline when displaying received text
/// in a SwiftUI `TextEditor` or sheet (#326).
pub const MAX_SHOWN_TEXT_CHARS: usize = 16 * 1024;

/// Canonical Universal Link prefix registered in `apple-app-site-association`.
pub const UNIVERSAL_LINK_PREFIX: &str = "https://totaldrop.app/r#";

/// Canonical custom URL scheme prefix registered in `Info.plist`.
pub const CUSTOM_SCHEME_PREFIX: &str = "totaldrop://r/";

/// Current state of the macOS main window when an external link or Finder share arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowActivity {
    /// Ready to start a new send or fill in a received code.
    Idle,
    /// A send or receive session is actively transferring or waiting.
    TransferRunning,
    /// An incoming offer sheet is waiting for the user to accept or decline.
    OfferPending,
    /// A received text note is currently on screen.
    ShowingReceivedText,
}

impl WindowActivity {
    /// Whether the Receive tab can immediately accept an incoming link code.
    #[must_use]
    pub const fn can_accept_link(self) -> bool {
        matches!(self, Self::Idle)
    }

    /// Whether the Send tab can immediately accept files shared from Finder.
    #[must_use]
    pub const fn can_accept_share(self) -> bool {
        matches!(self, Self::Idle)
    }
}

/// Outcome of routing an incoming `totaldrop://` URL or `https://totaldrop.app/r#…` Universal Link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MacLinkOutcome {
    /// Switch to the Receive view and connect using the parsed transfer code.
    OpenReceive {
        /// Human-readable English display form (`7-harbor-rocket`).
        display_code: String,
        /// Canonical index form passed to CPace (`7-840-1499`).
        canonical_code: String,
        /// Canonical Universal Link for resharing (`https://totaldrop.app/r#7-harbor-rocket`).
        universal_link: String,
    },
    /// Another operation owns the window; show a non-destructive notice instead.
    Busy {
        /// Localized `linkBusy` message for the active macOS language.
        message: String,
    },
    /// The URL is not a valid TotalDrop transfer link or code.
    Rejected,
}

/// Parses a URL delivered by `NSAppleEventManager` (`totaldrop://r/…`) or
/// `NSUserActivityTypeBrowsingWeb` (`https://totaldrop.app/r#…`) and decides
/// how the macOS app should respond.
#[must_use]
pub fn handle_macos_open_url(
    raw_url: &str,
    activity: WindowActivity,
    bundle: &MacLocalizationBundle,
) -> MacLinkOutcome {
    let Some(code) = TransferCode::from_link(raw_url) else {
        return MacLinkOutcome::Rejected;
    };
    if !activity.can_accept_link() {
        return MacLinkOutcome::Busy {
            message: bundle.text("linkBusy"),
        };
    }
    let display_code = code.to_english();
    let canonical_code = code.canonical();
    let universal_link = format!("{UNIVERSAL_LINK_PREFIX}{display_code}");
    MacLinkOutcome::OpenReceive {
        display_code,
        canonical_code,
        universal_link,
    }
}

/// Builds the Universal Link, custom scheme URL, and localized `NSSharingService`
/// share message for an active sender code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharePayload {
    /// Normalized English transfer code (`7-harbor-rocket`).
    pub code: String,
    /// Universal Link (`https://totaldrop.app/r#7-harbor-rocket`).
    pub universal_link: String,
    /// Custom scheme link (`totaldrop://r/7-harbor-rocket`).
    pub scheme_link: String,
    /// Localized share message (`sendShareText`) ready for AirDrop, Messages, or Mail.
    pub message: String,
}

/// Constructs a [`SharePayload`] for `code` in the active macOS language.
#[must_use]
pub fn build_share_payload(code: &TransferCode, bundle: &MacLocalizationBundle) -> SharePayload {
    let display = code.to_english();
    let universal_link = format!("{UNIVERSAL_LINK_PREFIX}{display}");
    let scheme_link = format!("{CUSTOM_SCHEME_PREFIX}{display}");
    let message = bundle
        .strings
        .get("sendShareText", &[("code", Arg::Text(display.as_str()))]);
    SharePayload {
        code: display,
        universal_link,
        scheme_link,
        message,
    }
}

/// Reason why an item handed over by Finder Quick Actions or `NSSharingService` was skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinderSkipReason {
    /// URL did not use the `file://` scheme or had a remote host.
    NotLocalFileUrl,
    /// Decoded path was empty or pointed to `/` with no file name.
    MissingFileName,
    /// File name failed `totaldrop_proto::validate_path` safety rules.
    UnsafeFileName,
    /// Another item in the same batch had the same case-insensitive top-level name.
    CaseCollision,
    /// Batch exceeded `totaldrop_proto::limits::MAX_ENTRIES`.
    TooManyEntries,
    /// Path did not exist or could not be read on disk.
    UnreadableOnDisk,
}

/// One validated local item accepted from Finder or drag-and-drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedFinderItem {
    /// Local filesystem path to read from.
    pub path: PathBuf,
    /// NFC-normalized safe display name (`SafePath` single-segment name).
    pub normalized_name: String,
    /// Case-folded collision key from [`SafePath::collision_key`].
    pub collision_key: String,
    /// Whether the path is a directory when checked on disk (`false` in syntax-only mode).
    pub is_directory: bool,
    /// Byte size when checked on disk (`0` for directories or syntax-only mode).
    pub byte_size: u64,
}

/// Result of validating a batch of `file://` URLs from Finder Quick Actions or `NSSharingService`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinderShareBatch {
    /// Accepted items in stable order.
    pub accepted: Vec<ValidatedFinderItem>,
    /// Rejected raw inputs paired with the reason they were skipped.
    pub skipped: Vec<(String, FinderSkipReason)>,
    /// Localized warning (`shareSkipped` or `shareBusy`), if any.
    pub notice: Option<String>,
    /// Whether the batch was rejected as a whole because a transfer was already running.
    pub busy: bool,
}

/// Decodes a `file://` URL (such as `file:///Users/ada/My%20File.pdf` or
/// `file://localhost/Users/ada/R%C3%A9sum%C3%A9.txt`) into a local [`PathBuf`].
#[must_use]
pub fn decode_macos_file_url(raw_url: &str) -> Option<PathBuf> {
    let trimmed = raw_url.trim();
    let after_scheme = trimmed.strip_prefix("file://")?;
    let raw_path = if let Some(rest) = after_scheme.strip_prefix("localhost/") {
        format!("/{rest}")
    } else if after_scheme.starts_with('/') {
        after_scheme.to_owned()
    } else {
        return None;
    };
    let decoded = percent_decode_utf8(&raw_path)?;
    if decoded.is_empty() || decoded.contains('\0') {
        return None;
    }
    Some(PathBuf::from(decoded))
}

fn hex_nibble(byte: u8) -> Option<u8> {
    char::from(byte)
        .to_digit(16)
        .and_then(|d| u8::try_from(d).ok())
}

fn percent_decode_utf8(input: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(input.len());
    let mut rest = input.as_bytes();
    while let Some((&b, tail)) = rest.split_first() {
        if b == b'%' {
            let [hi, lo, after @ ..] = tail else {
                return None;
            };
            let h = hex_nibble(*hi)?;
            let l = hex_nibble(*lo)?;
            let value = h.checked_mul(16).and_then(|v| v.checked_add(l))?;
            bytes.push(value);
            rest = after;
        } else {
            bytes.push(b);
            rest = tail;
        }
    }
    String::from_utf8(bytes).ok()
}

fn normalize_single_name(name: &str) -> Option<SafePath> {
    validate_path(&[name]).ok()
}

/// Validates a batch of `file://` URLs handed over by Finder Quick Actions,
/// `NSSharingService`, or SwiftUI `.dropDestination(for: URL.self)`.
///
/// When `verify_on_disk` is `true`, every candidate path must also exist and
/// have readable metadata (or readable file handle / directory listing).
#[must_use]
pub fn validate_finder_file_urls(
    raw_urls: &[String],
    activity: WindowActivity,
    verify_on_disk: bool,
    bundle: &MacLocalizationBundle,
) -> FinderShareBatch {
    if !activity.can_accept_share() {
        return FinderShareBatch {
            accepted: Vec::new(),
            skipped: Vec::new(),
            notice: Some(bundle.text("shareBusy")),
            busy: true,
        };
    }

    let mut accepted = Vec::new();
    let mut skipped = Vec::new();
    let mut seen_keys = BTreeSet::new();

    for raw in raw_urls {
        if accepted.len() >= limits::MAX_ENTRIES {
            skipped.push((raw.clone(), FinderSkipReason::TooManyEntries));
            continue;
        }
        let Some(path) = decode_macos_file_url(raw) else {
            skipped.push((raw.clone(), FinderSkipReason::NotLocalFileUrl));
            continue;
        };
        let Some(file_name) = path.file_name().and_then(|os| os.to_str()) else {
            skipped.push((raw.clone(), FinderSkipReason::MissingFileName));
            continue;
        };
        let Some(safe) = normalize_single_name(file_name) else {
            skipped.push((raw.clone(), FinderSkipReason::UnsafeFileName));
            continue;
        };
        let normalized_name = safe.segments().first().cloned().unwrap_or_default();
        let collision_key = safe.collision_key();
        if !seen_keys.insert(collision_key.clone()) {
            skipped.push((raw.clone(), FinderSkipReason::CaseCollision));
            continue;
        }

        let (is_directory, byte_size) = if verify_on_disk {
            match inspect_readable_path(&path) {
                Some(meta) => meta,
                None => {
                    seen_keys.remove(&collision_key);
                    skipped.push((raw.clone(), FinderSkipReason::UnreadableOnDisk));
                    continue;
                }
            }
        } else {
            (false, 0)
        };

        accepted.push(ValidatedFinderItem {
            path,
            normalized_name,
            collision_key,
            is_directory,
            byte_size,
        });
    }

    let notice = if skipped.is_empty() {
        None
    } else {
        let count = u64::try_from(skipped.len()).unwrap_or(u64::MAX);
        Some(
            bundle
                .strings
                .get("shareSkipped", &[("count", Arg::Count(count))]),
        )
    };

    FinderShareBatch {
        accepted,
        skipped,
        notice,
        busy: false,
    }
}

fn inspect_readable_path(path: &Path) -> Option<(bool, u64)> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() {
        // TotalDrop does not follow symlinks out of a shared selection.
        return None;
    }
    if metadata.is_dir() {
        let _iter = std::fs::read_dir(path).ok()?;
        Some((true, 0))
    } else if metadata.is_file() {
        let _file = std::fs::File::open(path).ok()?;
        Some((false, metadata.len()))
    } else {
        None
    }
}

/// Sanitized representation of received or peer-supplied text for SwiftUI views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizedDisplayText {
    /// Display-safe text with hidden controls shown as `<U+202E>`, capped at
    /// [`MAX_SHOWN_TEXT_CHARS`].
    pub display_text: String,
    /// Whether the text exceeded [`MAX_SHOWN_TEXT_CHARS`] and was shortened for display.
    pub shortened: bool,
    /// Whether hidden bidi or control characters were revealed.
    pub had_hidden_controls: bool,
}

/// Localization bundle bridging macOS `Locale.preferredLanguages` and `NSLocale`
/// regional formatting into `totaldrop-l10n` across all 35+ supported languages.
#[derive(Debug)]
pub struct MacLocalizationBundle {
    strings: Strings,
}

impl MacLocalizationBundle {
    /// Negotiates the best supported language from macOS `Locale.preferredLanguages`
    /// (BCP-47 tags such as `["zh-Hant-HK", "en-US"]` or `["tr-TR"]`) and an optional
    /// regional number-formatting tag (`NSLocale.current.identifier`, e.g. `"de_DE"`).
    #[must_use]
    pub fn from_macos_preferences(
        preferred_languages: &[String],
        numeric_locale: Option<&str>,
    ) -> Self {
        let prefs: Vec<_> = preferred_languages
            .iter()
            .filter_map(|tag| totaldrop_l10n::locale::parse(&normalize_apple_tag(tag)))
            .collect();
        let numeric = numeric_locale
            .map(normalize_apple_tag)
            .and_then(|tag| totaldrop_l10n::locale::parse(&tag));
        let strings = Strings::new(Domain::App, &prefs, numeric.as_ref());
        Self { strings }
    }

    /// Creates a bundle using the current POSIX/macOS environment variables.
    #[must_use]
    pub fn system() -> Self {
        Self {
            strings: Strings::system(Domain::App),
        }
    }

    /// All ARB locale tags compiled into `totaldrop-l10n` for the `App` domain.
    #[must_use]
    pub fn supported_locales() -> Vec<String> {
        totaldrop_l10n::available(Domain::App)
    }

    /// Chosen BCP-47 tag (for example `"en"`, `"tr"`, `"pt-BR"`, `"zh-Hant"`).
    #[must_use]
    pub fn tag(&self) -> &str {
        self.strings.tag()
    }

    /// Whether macOS SwiftUI views should apply `.environment(\.layoutDirection, .rightToLeft)`.
    #[must_use]
    pub fn is_rtl(&self) -> bool {
        self.strings.is_rtl()
    }

    /// Whether any active layer in this language contains unreviewed draft strings.
    #[must_use]
    pub fn has_drafts(&self) -> bool {
        self.strings.is_draft() || totaldrop_l10n::has_drafts(self.strings.tag())
    }

    /// Whether a specific key is a safety-critical string whose translation is still a draft.
    #[must_use]
    pub fn key_needs_english(&self, key: &str) -> bool {
        totaldrop_l10n::needs_english(self.strings.tag(), key)
    }

    /// Looks up a parameterless UI string.
    #[must_use]
    pub fn text(&self, key: &str) -> String {
        self.strings.get(key, &[])
    }

    /// Formats a byte count using regional SI units and decimal separators (`"1.5 MB"` / `"1,5 MB"`).
    #[must_use]
    pub fn format_bytes(&self, bytes: u64) -> String {
        self.strings.bytes(bytes)
    }

    /// Formats an integer with the region's digits and grouping.
    #[must_use]
    pub fn format_number(&self, value: u64) -> String {
        self.strings.number(value)
    }

    /// Reveals hidden bidi or control characters in a peer-supplied file name (`<U+202E>`).
    #[must_use]
    pub fn sanitize_filename(&self, name: &str) -> String {
        totaldrop_l10n::reveal_controls(name)
    }

    /// Sanitizes received text for safe display in SwiftUI, preserving `\n` and `\t`.
    #[must_use]
    pub fn sanitize_received_text(&self, raw: &str) -> SanitizedDisplayText {
        let revealed = totaldrop_l10n::reveal_text(raw);
        let had_hidden_controls = revealed != raw.replace("\r\n", "\n");
        match revealed.char_indices().nth(MAX_SHOWN_TEXT_CHARS) {
            Some((byte_idx, _)) => {
                let prefix = revealed.get(..byte_idx).unwrap_or_default().to_owned();
                SanitizedDisplayText {
                    display_text: prefix,
                    shortened: true,
                    had_hidden_controls,
                }
            }
            None => SanitizedDisplayText {
                display_text: revealed,
                shortened: false,
                had_hidden_controls,
            },
        }
    }

    /// Exports a JSON snapshot of resolved UI strings and metadata for SwiftUI initialization.
    #[must_use]
    pub fn export_swiftui_json(&self) -> String {
        let mut entries = serde_json::Map::new();
        for key in self.strings.keys() {
            entries.insert(
                key.to_owned(),
                serde_json::Value::String(self.strings.get(key, &[])),
            );
        }
        json!({
            "locale": self.tag(),
            "isRtl": self.is_rtl(),
            "isDraft": self.has_drafts(),
            "strings": entries,
        })
        .to_string()
    }
}

fn normalize_apple_tag(raw: &str) -> String {
    let base = raw.split('@').next().unwrap_or(raw);
    base.trim().replace('_', "-")
}

/// Regional transfer progress formatted for both the main macOS window and `MenuBarExtra`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormattedProgress {
    /// Clamped integer percentage `0..=100`.
    pub percent_value: u8,
    /// Localized percentage text (`progressPercent`, e.g. `"42%"` or `"%42"`).
    pub percent_text: String,
    /// Region-formatted transferred bytes (`"4.2 MB"` / `"4,2 MB"`).
    pub done_bytes_text: String,
    /// Region-formatted total bytes (`"10 MB"`).
    pub total_bytes_text: String,
    /// Combined detail line (`"4.2 MB / 10 MB"`).
    pub fraction_text: String,
    /// Compact status label for macOS `MenuBarExtra` (`"42% · 4.2 MB"`).
    pub menu_bar_label: String,
}

/// Computes regional transfer progress strings from `done` and `total` byte counts.
#[must_use]
pub fn format_transfer_progress(
    done: u64,
    total: u64,
    bundle: &MacLocalizationBundle,
) -> FormattedProgress {
    let clamped_done = if total > 0 { done.min(total) } else { done };
    let percent_u64 = if total == 0 {
        if done == 0 { 0 } else { 100 }
    } else {
        u128::from(clamped_done)
            .saturating_mul(100)
            .checked_div(u128::from(total))
            .and_then(|v| u64::try_from(v).ok())
            .unwrap_or(100)
            .min(100)
    };
    let percent_value = u8::try_from(percent_u64).unwrap_or(100);
    let percent_text = bundle
        .strings
        .get("progressPercent", &[("percent", Arg::Count(percent_u64))]);
    let done_bytes_text = bundle.format_bytes(clamped_done);
    let total_bytes_text = bundle.format_bytes(total);
    let fraction_text = format!("{done_bytes_text} / {total_bytes_text}");
    let menu_bar_label = format!("{percent_text} · {done_bytes_text}");
    FormattedProgress {
        percent_value,
        percent_text,
        done_bytes_text,
        total_bytes_text,
        fraction_text,
        menu_bar_label,
    }
}

/// Formats an item count and total byte size (`itemsAndSize`) for the Send selection bar.
#[must_use]
pub fn format_selection_summary(
    item_count: usize,
    total_bytes: u64,
    bundle: &MacLocalizationBundle,
) -> String {
    let count = u64::try_from(item_count).unwrap_or(u64::MAX);
    let size = bundle.format_bytes(total_bytes);
    bundle.strings.get(
        "itemsAndSize",
        &[("count", Arg::Count(count)), ("size", Arg::Text(&size))],
    )
}

/// Network and privacy configuration for macOS transfers.
#[derive(Debug, Clone)]
pub struct MacTransferConfig {
    /// Rendezvous WebSocket URL (`wss://…`).
    pub server: String,
    /// Relay WebSocket URL (`wss://…`), or empty to disable relay fallback.
    pub relay: String,
    /// STUN server (`host:port`), or empty to disable STUN discovery.
    pub stun: String,
    /// Privacy Mode (`relay_only`): never announce direct addresses or ask UPnP/PCP.
    pub privacy_mode: bool,
    /// Whether to generate a 3-word transfer code instead of 2 words.
    pub stronger_code: bool,
}

impl Default for MacTransferConfig {
    fn default() -> Self {
        let paths = PathOptions::default();
        Self {
            server: totaldrop_core::rendezvous::DEFAULT_SERVER.to_owned(),
            relay: paths.relay,
            stun: paths.stun,
            privacy_mode: false,
            stronger_code: false,
        }
    }
}

impl MacTransferConfig {
    /// Converts this configuration into [`totaldrop_core::PathOptions`].
    #[must_use]
    pub fn path_options(&self) -> PathOptions {
        PathOptions {
            relay: self.relay.clone(),
            stun: self.stun.clone(),
            relay_only: self.privacy_mode,
            gateway: if self.privacy_mode {
                totaldrop_core::portmap::Gateway::Off
            } else {
                totaldrop_core::portmap::Gateway::Auto
            },
        }
    }

    /// Converts this configuration into [`totaldrop_core::SendOptions`].
    #[must_use]
    pub fn send_options(&self) -> SendOptions {
        SendOptions {
            server: self.server.clone(),
            words: if self.stronger_code { 3 } else { 2 },
            paths: self.path_options(),
        }
    }

    /// Converts this configuration into [`totaldrop_core::ReceiveOptions`].
    #[must_use]
    pub fn receive_options(&self) -> ReceiveOptions {
        ReceiveOptions {
            server: self.server.clone(),
            paths: self.path_options(),
        }
    }
}

/// Thread-safe handle allowing SwiftUI views or `MenuBarExtra` to cancel an in-flight transfer.
#[derive(Clone, Default)]
pub struct MacStopHandle {
    inner: Stop,
}

impl std::fmt::Debug for MacStopHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MacStopHandle")
            .field("stopped", &self.inner.is_stopped())
            .finish()
    }
}

impl MacStopHandle {
    /// Creates an active, unstopped handle.
    #[must_use]
    pub fn new() -> Self {
        Self { inner: Stop::new() }
    }

    /// Signals cancellation; safe to call multiple times from any thread.
    pub fn cancel(&self) {
        self.inner.stop();
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.is_stopped()
    }

    /// Underlying [`totaldrop_core::Stop`] reference.
    #[must_use]
    pub const fn as_core_stop(&self) -> &Stop {
        &self.inner
    }
}

/// Localized error presentation ready for a macOS `Alert` or inline banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacErrorAlert {
    /// ARB headline key selected for this failure (`errorWrongCode`, `errorDeclined`, …).
    pub headline_key: &'static str,
    /// Localized headline message.
    pub headline: String,
    /// Optional localized plain-language cause (`causeTimedOut`, `causeNoRoute`, …).
    pub cause: Option<String>,
    /// Sanitized English technical detail for bug reports.
    pub technical_detail: String,
}

/// Maps a [`totaldrop_core::Error`] to localized macOS alert strings.
#[must_use]
pub fn describe_core_error(
    error: &CoreError,
    is_sender: bool,
    bundle: &MacLocalizationBundle,
) -> MacErrorAlert {
    let headline_key = match error {
        CoreError::WrongCode => "errorWrongCode",
        CoreError::PeerLeft => "errorPeerLeft",
        CoreError::Rejected(_) => "errorDeclined",
        CoreError::Cancelled(_) => "errorCancelled",
        CoreError::Stopped => "errorStopped",
        CoreError::Code(_) => "errorInvalidCode",
        CoreError::Rendezvous(_) if is_sender && error.is_code_not_in_use() => "errorCodeExpired",
        CoreError::Rendezvous(_) if !is_sender && error.is_code_not_in_use() => "errorCodeNotInUse",
        CoreError::Rendezvous(_) if error.is_bad_server_address() => "errorBadServer",
        CoreError::Rendezvous(_) => "errorServer",
        CoreError::Connect(_) => "errorConnect",
        CoreError::InvalidOffer(_) => "errorInvalidOffer",
        CoreError::Io(_) => "errorStorage",
        CoreError::Protocol(_) | CoreError::Random => "errorOther",
    };
    let headline = bundle.text(headline_key);
    let cause_suffix = error.cause().as_str();
    let cause = if cause_suffix.is_empty() {
        None
    } else {
        Some(bundle.text(&format!("cause{cause_suffix}")))
    };
    let technical_detail = bundle.sanitize_filename(&error.to_string());
    MacErrorAlert {
        headline_key,
        headline,
        cause,
        technical_detail,
    }
}

/// Prepares a file-transfer offer via [`totaldrop_core::PreparedSend::new`].
///
/// # Errors
///
/// Returns a [`totaldrop_core::Error`] if hashing fails, the rendezvous server
/// cannot allocate a nameplate, or `stop` cancels preparation.
pub async fn prepare_macos_file_send(
    config: &MacTransferConfig,
    files: &[PathBuf],
    stop: &MacStopHandle,
) -> totaldrop_core::Result<PreparedSend> {
    if stop.is_cancelled() {
        return Err(CoreError::Stopped);
    }
    let opts = config.send_options();
    let paths = files.to_vec();
    tokio::select! {
        biased;
        res = PreparedSend::new(&opts, paths) => res,
        () = stop.as_core_stop().stopped() => Err(CoreError::Stopped),
    }
}

/// Connects to a sender's mailbox and fetches the incoming offer via [`totaldrop_core::IncomingOffer::connect_with`].
///
/// # Errors
///
/// Returns a [`totaldrop_core::Error`] if the transfer code is invalid, CPace
/// key confirmation fails, no network route succeeds, or `stop` cancels connection.
pub async fn connect_macos_receive(
    config: &MacTransferConfig,
    code_or_link: &str,
    stop: &MacStopHandle,
) -> totaldrop_core::Result<IncomingOffer> {
    if stop.is_cancelled() {
        return Err(CoreError::Stopped);
    }
    let code = TransferCode::from_link(code_or_link)
        .ok_or(CoreError::Code(totaldrop_proto::CodeError::BadNameplate))?;
    IncomingOffer::connect_with(
        &config.receive_options(),
        &code.to_english(),
        stop.as_core_stop(),
    )
    .await
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::*;

    fn english_bundle() -> MacLocalizationBundle {
        MacLocalizationBundle::from_macos_preferences(&["en-US".to_owned()], Some("en_US"))
    }

    #[test]
    fn universal_link_routes_to_receive_when_idle() {
        let bundle = english_bundle();
        let outcome = handle_macos_open_url(
            "https://totaldrop.app/r#7-harbor-rocket",
            WindowActivity::Idle,
            &bundle,
        );
        assert_eq!(
            outcome,
            MacLinkOutcome::OpenReceive {
                display_code: "7-harbor-rocket".to_owned(),
                canonical_code: TransferCode::parse("7-harbor-rocket").unwrap().canonical(),
                universal_link: "https://totaldrop.app/r#7-harbor-rocket".to_owned(),
            }
        );
    }

    #[test]
    fn custom_scheme_link_decodes_percent_escapes_and_strips_bidi_marks() {
        let bundle = english_bundle();
        let outcome = handle_macos_open_url(
            "\u{2068}totaldrop://r/12-abandon%2Dability\u{2069}",
            WindowActivity::Idle,
            &bundle,
        );
        match outcome {
            MacLinkOutcome::OpenReceive { display_code, .. } => {
                assert_eq!(display_code, "12-abandon-ability");
            }
            other => panic!("expected OpenReceive, got {other:?}"),
        }
    }

    #[test]
    fn link_handler_reports_busy_when_transfer_is_running() {
        let bundle = MacLocalizationBundle::from_macos_preferences(&["tr-TR".to_owned()], None);
        let outcome = handle_macos_open_url(
            "https://totaldrop.app/r#7-harbor-rocket",
            WindowActivity::TransferRunning,
            &bundle,
        );
        match outcome {
            MacLinkOutcome::Busy { message } => {
                assert!(!message.is_empty());
                assert_ne!(message, "linkBusy");
            }
            other => panic!("expected Busy, got {other:?}"),
        }
    }

    #[test]
    fn link_handler_rejects_untrusted_domains_and_oversized_urls() {
        let bundle = english_bundle();
        assert_eq!(
            handle_macos_open_url(
                "https://example.com/r#7-harbor-rocket",
                WindowActivity::Idle,
                &bundle,
            ),
            MacLinkOutcome::Rejected
        );
        let huge = format!("totaldrop://r/{}", "a".repeat(limits::MAX_PATH_BYTES));
        assert_eq!(
            handle_macos_open_url(&huge, WindowActivity::Idle, &bundle),
            MacLinkOutcome::Rejected
        );
    }

    #[test]
    fn share_payload_round_trips_back_into_transfer_code() {
        let code = TransferCode::parse("9-harbor-rocket").unwrap();
        for lang in ["en-US", "tr-TR", "ar-SA", "ja-JP", "zh-Hant-TW"] {
            let bundle = MacLocalizationBundle::from_macos_preferences(&[lang.to_owned()], None);
            let payload = build_share_payload(&code, &bundle);
            assert_eq!(
                TransferCode::from_link(&payload.universal_link).as_ref(),
                Some(&code)
            );
            assert_eq!(
                TransferCode::from_link(&payload.scheme_link).as_ref(),
                Some(&code)
            );
            assert_eq!(TransferCode::find(&payload.message).as_ref(), Some(&code));
        }
    }

    #[test]
    fn finder_file_url_decoder_normalizes_apfs_nfd_and_spaces() {
        let bundle = english_bundle();
        // APFS often hands decomposed NFD names ("Cafe\u{0301} Notes.pdf").
        let urls = vec![
            "file:///Users/ada/Desktop/Cafe%CC%81%20Notes.pdf".to_owned(),
            "file://localhost/Users/ada/Desktop/Archive.tar.gz".to_owned(),
        ];
        let batch = validate_finder_file_urls(&urls, WindowActivity::Idle, false, &bundle);
        assert_eq!(batch.accepted.len(), 2);
        assert!(batch.skipped.is_empty());
        assert_eq!(batch.accepted[0].normalized_name, "Café Notes.pdf");
        assert_eq!(batch.accepted[1].normalized_name, "Archive.tar.gz");
    }

    #[test]
    fn finder_validator_rejects_case_collisions_and_remote_urls() {
        let bundle = english_bundle();
        let urls = vec![
            "file:///Users/ada/FolderA/Report.pdf".to_owned(),
            "file:///Users/ada/FolderB/report.pdf".to_owned(),
            "https://totaldrop.app/not-a-file".to_owned(),
            "file:///Users/ada/CON.txt".to_owned(),
        ];
        let batch = validate_finder_file_urls(&urls, WindowActivity::Idle, false, &bundle);
        assert_eq!(batch.accepted.len(), 1);
        assert_eq!(batch.accepted[0].normalized_name, "Report.pdf");
        assert_eq!(batch.skipped.len(), 3);
        assert_eq!(batch.skipped[0].1, FinderSkipReason::CaseCollision);
        assert_eq!(batch.skipped[1].1, FinderSkipReason::NotLocalFileUrl);
        assert_eq!(batch.skipped[2].1, FinderSkipReason::UnsafeFileName);
        assert!(batch.notice.is_some());
    }

    #[test]
    fn finder_validator_checks_on_disk_readability_and_busy_window() {
        let bundle = english_bundle();
        let temp_dir = std::env::temp_dir().join("totaldrop-macos-bridge-finder-test");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();
        let real_file = temp_dir.join("hello.txt");
        std::fs::write(&real_file, b"hello macos").unwrap();

        let ok_url = format!("file://{}", real_file.display());
        let missing_url = format!("file://{}/does-not-exist.txt", temp_dir.display());
        let batch = validate_finder_file_urls(
            &[ok_url.clone(), missing_url],
            WindowActivity::Idle,
            true,
            &bundle,
        );
        assert_eq!(batch.accepted.len(), 1);
        assert_eq!(batch.accepted[0].byte_size, 11);
        assert_eq!(batch.skipped.len(), 1);
        assert_eq!(batch.skipped[0].1, FinderSkipReason::UnreadableOnDisk);

        let busy_batch =
            validate_finder_file_urls(&[ok_url], WindowActivity::OfferPending, true, &bundle);
        assert!(busy_batch.busy);
        assert!(busy_batch.accepted.is_empty());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn macos_localization_bundle_covers_all_35_plus_languages_and_rtl() {
        let locales = MacLocalizationBundle::supported_locales();
        assert!(
            locales.len() >= 35,
            "expected at least 35 app locales, got {}",
            locales.len()
        );

        let zh_hant = MacLocalizationBundle::from_macos_preferences(
            &["zh-Hant-HK".to_owned(), "en-US".to_owned()],
            Some("zh_HK"),
        );
        assert_eq!(zh_hant.tag(), "zh-Hant");
        assert!(!zh_hant.is_rtl());

        let arabic = MacLocalizationBundle::from_macos_preferences(&["ar-EG".to_owned()], None);
        assert_eq!(arabic.tag(), "ar");
        assert!(arabic.is_rtl());

        let fallback = MacLocalizationBundle::from_macos_preferences(&["tlh-KL".to_owned()], None);
        assert_eq!(fallback.tag(), "en");
    }

    #[test]
    fn regional_progress_formats_decimal_separators_and_clamps_bounds() {
        let us =
            MacLocalizationBundle::from_macos_preferences(&["en-US".to_owned()], Some("en_US"));
        let de =
            MacLocalizationBundle::from_macos_preferences(&["de-DE".to_owned()], Some("de_DE"));

        let p_us = format_transfer_progress(1_500_000, 3_000_000, &us);
        assert_eq!(p_us.percent_value, 50);
        assert_eq!(p_us.done_bytes_text, "1.5 MB");
        assert!(p_us.menu_bar_label.contains("50%"));

        let p_de = format_transfer_progress(1_500_000, 3_000_000, &de);
        assert_eq!(p_de.percent_value, 50);
        assert_eq!(p_de.done_bytes_text, "1,5 MB");

        let zero = format_transfer_progress(0, 0, &us);
        assert_eq!(zero.percent_value, 0);

        let overflow = format_transfer_progress(5_000, 1_000, &us);
        assert_eq!(overflow.percent_value, 100);
    }

    #[test]
    fn sanitize_received_text_names_bidi_controls_and_caps_length() {
        let bundle = english_bundle();
        let spoofed = bundle.sanitize_received_text("invoice\u{202E}fdp.dmg\r\nsecond\tline");
        assert_eq!(spoofed.display_text, "invoice<U+202E>fdp.dmg\nsecond\tline");
        assert!(spoofed.had_hidden_controls);
        assert!(!spoofed.shortened);

        let long_input = "m".repeat(MAX_SHOWN_TEXT_CHARS + 32);
        let truncated = bundle.sanitize_received_text(&long_input);
        assert!(truncated.shortened);
        assert_eq!(truncated.display_text.chars().count(), MAX_SHOWN_TEXT_CHARS);
    }

    #[test]
    fn stopper_and_error_mapping_preserve_privacy_and_localize_headlines() {
        let bundle = english_bundle();
        let cfg = MacTransferConfig {
            privacy_mode: true,
            stronger_code: true,
            ..Default::default()
        };
        let send_opts = cfg.send_options();
        assert!(send_opts.paths.relay_only);
        assert_eq!(send_opts.words, 3);

        let stop = MacStopHandle::new();
        assert!(!stop.is_cancelled());
        stop.cancel();
        assert!(stop.is_cancelled());

        let wrong = describe_core_error(&CoreError::WrongCode, false, &bundle);
        assert_eq!(wrong.headline_key, "errorWrongCode");
        assert!(wrong.headline.contains("code did not match"));

        let snapshot: serde_json::Value =
            serde_json::from_str(&bundle.export_swiftui_json()).unwrap();
        assert_eq!(snapshot["locale"], "en");
        assert_eq!(snapshot["isRtl"], false);
    }
}
