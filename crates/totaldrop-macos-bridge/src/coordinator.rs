// SPDX-FileCopyrightText: 2026 The TotalDrop Contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//
//! Stateful session coordinator and deterministic UTF-8 JSON projections for
//! `TotalDropMac` (`WindowGroup` + `MenuBarExtra`) and `IOPMAssertion` power
//! management.

use serde_json::{Map, Value};
use totaldrop_core::{Error as CoreError, Event, OfferSummary};
use totaldrop_l10n::Arg;
use totaldrop_proto::TransferCode;

use crate::{
    FormattedProgress, MacErrorAlert, MacLocalizationBundle, MacStopHandle, UNIVERSAL_LINK_PREFIX,
    WindowActivity, describe_core_error, format_transfer_progress,
};

/// Maximum number of sanitized top-level names rendered inline in the macOS
/// incoming-offer sheet before collapsing the remainder into `receiveAndMore`.
pub const MAX_OFFER_PREVIEW_NAMES: usize = 10;

/// Direction of an active macOS transfer session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacSessionRole {
    /// Sending local files, folders, or inline text from this Mac.
    Sender,
    /// Receiving offered files, folders, or inline text onto this Mac.
    Receiver,
}

impl MacSessionRole {
    /// Stable lowercase identifier serialized into Swift 6 JSON snapshots.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sender => "sender",
            Self::Receiver => "receiver",
        }
    }
}

/// Lifecycle phase of a macOS transfer session mirrored into SwiftUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacSessionPhase {
    /// No active session; ready for new selections or links.
    Idle,
    /// Hashing files and allocating a rendezvous nameplate.
    Preparing,
    /// Sender code is ready and waiting for a receiver (`Event::Waiting`).
    WaitingForPeer,
    /// Receiver is dialing the sender's mailbox and negotiating transport.
    Connecting,
    /// Receiver fetched a verified manifest and awaits user confirmation.
    OfferPending,
    /// Encrypted channel is established (`Event::Connected`) before first byte tick.
    Connected,
    /// Encrypted payload chunks are actively moving (`Event::Progress`).
    Transferring,
    /// All entries finished and passed BLAKE3 verification.
    Completed,
    /// Session ended with an error or peer rejection.
    Failed,
}

impl MacSessionPhase {
    /// Stable camelCase phase tag consumed by `TotalDropMac` Swift 6 decoders.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Preparing => "preparing",
            Self::WaitingForPeer => "waitingForPeer",
            Self::Connecting => "connecting",
            Self::OfferPending => "offerPending",
            Self::Connected => "connected",
            Self::Transferring => "transferring",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    /// Whether the session owns the active transfer slot and blocks incoming links/shares.
    #[must_use]
    pub const fn is_busy(self) -> bool {
        !matches!(self, Self::Idle | Self::Completed | Self::Failed)
    }

    /// Whether macOS should hold `kIOPMAssertionTypePreventUserIdleSystemSleep`.
    #[must_use]
    pub const fn requires_sleep_assertion(self) -> bool {
        self.is_busy()
    }

    /// Maps this session phase to [`WindowActivity`] for link and Finder-share routing.
    #[must_use]
    pub const fn window_activity(self) -> WindowActivity {
        match self {
            Self::Idle | Self::Completed | Self::Failed => WindowActivity::Idle,
            Self::OfferPending => WindowActivity::OfferPending,
            Self::Preparing
            | Self::WaitingForPeer
            | Self::Connecting
            | Self::Connected
            | Self::Transferring => WindowActivity::TransferRunning,
        }
    }
}

/// Sanitized preview of an incoming offer for the macOS confirmation card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacOfferPreview {
    /// Count of regular files in the manifest.
    pub files: usize,
    /// Count of directories in the manifest.
    pub dirs: usize,
    /// Total byte length across all offered files.
    pub total_bytes: u64,
    /// Region-formatted total byte size (`"4.2 MB"` / `"4,2 MB"`).
    pub formatted_size: String,
    /// Whether this offer carries an inline text note rather than saved files.
    pub is_text_offer: bool,
    /// Localized offer card title (`receiveOffer` or `receiveOfferText`).
    pub headline: String,
    /// Localized file/size summary line (`receiveOfferSummary` or `receiveOfferTextNote`).
    pub summary_line: String,
    /// Up to [`MAX_OFFER_PREVIEW_NAMES`] sanitized top-level names with hidden controls
    /// revealed as `<U+XXXX>` (for example `<U+202E>`).
    pub top_level_names: Vec<String>,
    /// Number of additional top-level items omitted after [`MAX_OFFER_PREVIEW_NAMES`].
    pub remaining_names: usize,
    /// Localized `"and {count} more"` footer when `remaining_names > 0`.
    pub more_names_line: Option<String>,
    /// `true` if any peer-supplied top-level name contained hidden bidi or control characters.
    pub has_hidden_controls: bool,
}

impl MacOfferPreview {
    /// Projects a verified [`OfferSummary`] into localized, bidi-safe macOS display strings.
    #[must_use]
    pub fn from_offer_summary(summary: &OfferSummary, bundle: &MacLocalizationBundle) -> Self {
        let formatted_size = bundle.format_bytes(summary.total_size);
        let headline = if summary.text {
            bundle.text("receiveOfferText")
        } else {
            bundle.text("receiveOffer")
        };
        let summary_line = if summary.text {
            bundle.text("receiveOfferTextNote")
        } else {
            let file_count = u64::try_from(summary.files).unwrap_or(u64::MAX);
            bundle.strings.get(
                "receiveOfferSummary",
                &[
                    ("files", Arg::Count(file_count)),
                    ("size", Arg::Text(&formatted_size)),
                ],
            )
        };

        let mut has_hidden_controls = false;
        let preview_cap = summary.top_level.len().min(MAX_OFFER_PREVIEW_NAMES);
        let mut top_level_names = Vec::with_capacity(preview_cap);
        for (idx, raw_name) in summary.top_level.iter().enumerate() {
            let sanitized = bundle.sanitize_filename(raw_name);
            if sanitized != *raw_name {
                has_hidden_controls = true;
            }
            if idx < MAX_OFFER_PREVIEW_NAMES {
                top_level_names.push(sanitized);
            }
        }

        let remaining_names = summary
            .top_level
            .len()
            .saturating_sub(top_level_names.len());
        let more_names_line = if remaining_names > 0 {
            let more_count = u64::try_from(remaining_names).unwrap_or(u64::MAX);
            Some(
                bundle
                    .strings
                    .get("receiveAndMore", &[("count", Arg::Count(more_count))]),
            )
        } else {
            None
        };

        Self {
            files: summary.files,
            dirs: summary.dirs,
            total_bytes: summary.total_size,
            formatted_size,
            is_text_offer: summary.text,
            headline,
            summary_line,
            top_level_names,
            remaining_names,
            more_names_line,
            has_hidden_controls,
        }
    }

    /// Converts this offer preview into a deterministic JSON object (`BTreeMap`-ordered keys).
    #[must_use]
    pub fn to_json_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("dirs".to_owned(), Value::from(self.dirs));
        map.insert("files".to_owned(), Value::from(self.files));
        map.insert(
            "formattedSize".to_owned(),
            Value::String(self.formatted_size.clone()),
        );
        map.insert(
            "hasHiddenControls".to_owned(),
            Value::Bool(self.has_hidden_controls),
        );
        map.insert("headline".to_owned(), Value::String(self.headline.clone()));
        map.insert("isTextOffer".to_owned(), Value::Bool(self.is_text_offer));
        map.insert(
            "moreNamesLine".to_owned(),
            match &self.more_names_line {
                Some(line) => Value::String(line.clone()),
                None => Value::Null,
            },
        );
        map.insert(
            "remainingNames".to_owned(),
            Value::from(self.remaining_names),
        );
        map.insert(
            "summaryLine".to_owned(),
            Value::String(self.summary_line.clone()),
        );
        map.insert(
            "topLevelNames".to_owned(),
            Value::Array(
                self.top_level_names
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
        map.insert("totalBytes".to_owned(), Value::from(self.total_bytes));
        Value::Object(map)
    }

    /// Serializes this offer preview as deterministic UTF-8 JSON.
    #[must_use]
    pub fn to_json(&self) -> String {
        self.to_json_value().to_string()
    }
}

/// Complete macOS UI & `MenuBarExtra` state snapshot projected for Swift 6 consumption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacSessionSnapshot {
    /// Active role (`Sender` or `Receiver`).
    pub role: MacSessionRole,
    /// Current transfer lifecycle phase.
    pub phase: MacSessionPhase,
    /// Negotiated BCP-47 locale tag (`"en"`, `"tr"`, `"ar"`, …).
    pub locale: String,
    /// Whether SwiftUI views should apply right-to-left layout direction.
    pub is_rtl: bool,
    /// Busy-window classification for external URL and Finder share routing.
    pub window_activity: WindowActivity,
    /// Whether `IOPMAssertion` idle-sleep prevention must be held right now.
    pub requires_sleep_assertion: bool,
    /// Localized reason string passed to `IOPMAssertionCreateWithName` (`keepAwakeReason`).
    pub sleep_assertion_reason: String,
    /// Compact label for the macOS `MenuBarExtra` status item.
    pub menu_bar_status: String,
    /// Localized primary status headline for the main window.
    pub status_headline: String,
    /// Normalized English transfer code (`"7-harbor-rocket"`), if assigned.
    pub display_code: Option<String>,
    /// Canonical Universal Link (`"https://totaldrop.app/r#7-harbor-rocket"`), if assigned.
    pub universal_link: Option<String>,
    /// Four English verification words once the encrypted channel is established.
    pub verification_words: Vec<String>,
    /// Clamped transferred byte count (`0..=total_bytes` when `total_bytes > 0`).
    pub done_bytes: u64,
    /// Total expected byte count across the session.
    pub total_bytes: u64,
    /// Regional progress strings when `Event::Progress` has reported bytes.
    pub progress: Option<FormattedProgress>,
    /// Incoming offer summary when receiving and awaiting/processing an offer.
    pub offer: Option<MacOfferPreview>,
    /// Localized alert metadata when `phase` is [`MacSessionPhase::Failed`].
    pub error: Option<MacErrorAlert>,
}

impl MacSessionSnapshot {
    /// Projects a single [`totaldrop_core::Event`] directly into a [`MacSessionSnapshot`].
    #[must_use]
    pub fn from_core_event(
        role: MacSessionRole,
        event: &Event,
        code: Option<&TransferCode>,
        prior_words: &[String],
        bundle: &MacLocalizationBundle,
    ) -> Self {
        let mut coordinator = MacSessionCoordinator::new(role);
        if let Some(c) = code {
            coordinator.set_code(c);
        }
        if !prior_words.is_empty() {
            coordinator.verification_words = prior_words.to_vec();
        }
        coordinator.apply_event(event, bundle)
    }

    /// Converts this snapshot into a deterministic JSON value (`BTreeMap`-sorted keys).
    #[must_use]
    pub fn to_json_value(&self) -> Value {
        let mut map = Map::new();
        map.insert(
            "displayCode".to_owned(),
            match &self.display_code {
                Some(code) => Value::String(code.clone()),
                None => Value::Null,
            },
        );
        map.insert("doneBytes".to_owned(), Value::from(self.done_bytes));
        map.insert(
            "error".to_owned(),
            match &self.error {
                Some(err) => {
                    let mut err_map = Map::new();
                    err_map.insert(
                        "cause".to_owned(),
                        match &err.cause {
                            Some(c) => Value::String(c.clone()),
                            None => Value::Null,
                        },
                    );
                    err_map.insert("headline".to_owned(), Value::String(err.headline.clone()));
                    err_map.insert(
                        "headlineKey".to_owned(),
                        Value::String(err.headline_key.to_owned()),
                    );
                    err_map.insert(
                        "technicalDetail".to_owned(),
                        Value::String(err.technical_detail.clone()),
                    );
                    Value::Object(err_map)
                }
                None => Value::Null,
            },
        );
        map.insert("isBusy".to_owned(), Value::Bool(self.phase.is_busy()));
        map.insert("isRtl".to_owned(), Value::Bool(self.is_rtl));
        map.insert("locale".to_owned(), Value::String(self.locale.clone()));
        map.insert(
            "menuBarStatus".to_owned(),
            Value::String(self.menu_bar_status.clone()),
        );
        map.insert(
            "offer".to_owned(),
            match &self.offer {
                Some(offer) => offer.to_json_value(),
                None => Value::Null,
            },
        );
        map.insert(
            "phase".to_owned(),
            Value::String(self.phase.as_str().to_owned()),
        );
        map.insert(
            "progress".to_owned(),
            match &self.progress {
                Some(p) => {
                    let mut prog_map = Map::new();
                    prog_map.insert(
                        "doneBytesText".to_owned(),
                        Value::String(p.done_bytes_text.clone()),
                    );
                    prog_map.insert(
                        "fractionText".to_owned(),
                        Value::String(p.fraction_text.clone()),
                    );
                    prog_map.insert(
                        "menuBarLabel".to_owned(),
                        Value::String(p.menu_bar_label.clone()),
                    );
                    prog_map.insert(
                        "percentText".to_owned(),
                        Value::String(p.percent_text.clone()),
                    );
                    prog_map.insert("percentValue".to_owned(), Value::from(p.percent_value));
                    prog_map.insert(
                        "totalBytesText".to_owned(),
                        Value::String(p.total_bytes_text.clone()),
                    );
                    Value::Object(prog_map)
                }
                None => Value::Null,
            },
        );
        map.insert(
            "requiresSleepAssertion".to_owned(),
            Value::Bool(self.requires_sleep_assertion),
        );
        map.insert(
            "role".to_owned(),
            Value::String(self.role.as_str().to_owned()),
        );
        map.insert(
            "sleepAssertionReason".to_owned(),
            Value::String(self.sleep_assertion_reason.clone()),
        );
        map.insert(
            "statusHeadline".to_owned(),
            Value::String(self.status_headline.clone()),
        );
        map.insert("totalBytes".to_owned(), Value::from(self.total_bytes));
        map.insert(
            "universalLink".to_owned(),
            match &self.universal_link {
                Some(link) => Value::String(link.clone()),
                None => Value::Null,
            },
        );
        map.insert(
            "verificationWords".to_owned(),
            Value::Array(
                self.verification_words
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
        Value::Object(map)
    }

    /// Serializes this session snapshot to deterministic UTF-8 JSON for Swift 6 FFI/IPC.
    #[must_use]
    pub fn to_json(&self) -> String {
        self.to_json_value().to_string()
    }
}

/// Stateful macOS bridge coordinator tracking [`Event`] transitions, incoming offers,
/// `MenuBarExtra` status strings, `IOPMAssertion` requirements, and [`MacStopHandle`].
#[derive(Debug, Clone)]
pub struct MacSessionCoordinator {
    role: MacSessionRole,
    phase: MacSessionPhase,
    display_code: Option<String>,
    universal_link: Option<String>,
    verification_words: Vec<String>,
    done_bytes: u64,
    total_bytes: u64,
    progress: Option<FormattedProgress>,
    offer: Option<MacOfferPreview>,
    error: Option<MacErrorAlert>,
    stop: MacStopHandle,
}

impl MacSessionCoordinator {
    /// Creates an idle coordinator for a new send or receive flow.
    #[must_use]
    pub fn new(role: MacSessionRole) -> Self {
        Self {
            role,
            phase: MacSessionPhase::Idle,
            display_code: None,
            universal_link: None,
            verification_words: Vec::new(),
            done_bytes: 0,
            total_bytes: 0,
            progress: None,
            offer: None,
            error: None,
            stop: MacStopHandle::new(),
        }
    }

    /// Role assigned to this coordinator (`Sender` or `Receiver`).
    #[must_use]
    pub const fn role(&self) -> MacSessionRole {
        self.role
    }

    /// Current session lifecycle phase.
    #[must_use]
    pub const fn phase(&self) -> MacSessionPhase {
        self.phase
    }

    /// Shared cancellation handle wired to SwiftUI `Cancel`/`Stop` and `MenuBarExtra`.
    #[must_use]
    pub const fn stop_handle(&self) -> &MacStopHandle {
        &self.stop
    }

    /// Associates a validated [`TransferCode`] with this session, populating its
    /// normalized English display form and canonical Universal Link.
    pub fn set_code(&mut self, code: &TransferCode) {
        let display = code.to_english();
        self.universal_link = Some(format!("{UNIVERSAL_LINK_PREFIX}{display}"));
        self.display_code = Some(display);
    }

    /// Enters [`MacSessionPhase::Preparing`] while scanning files and reserving a nameplate.
    pub fn begin_preparing(&mut self, bundle: &MacLocalizationBundle) -> MacSessionSnapshot {
        self.phase = MacSessionPhase::Preparing;
        self.error = None;
        self.progress = None;
        self.done_bytes = 0;
        self.snapshot(bundle)
    }

    /// Enters [`MacSessionPhase::Connecting`] for a receiver joining `code`.
    pub fn begin_connecting(
        &mut self,
        code: &TransferCode,
        bundle: &MacLocalizationBundle,
    ) -> MacSessionSnapshot {
        self.set_code(code);
        self.phase = MacSessionPhase::Connecting;
        self.error = None;
        self.progress = None;
        self.done_bytes = 0;
        self.snapshot(bundle)
    }

    /// Applies a [`totaldrop_core::Event`] (`Waiting`, `Connected`, or `Progress`) and returns
    /// the updated [`MacSessionSnapshot`].
    pub fn apply_event(
        &mut self,
        event: &Event,
        bundle: &MacLocalizationBundle,
    ) -> MacSessionSnapshot {
        self.error = None;
        match event {
            Event::Waiting => {
                self.phase = MacSessionPhase::WaitingForPeer;
                self.done_bytes = 0;
                self.progress = None;
            }
            Event::Connected { words } => {
                self.phase = MacSessionPhase::Connected;
                self.verification_words = words.clone();
            }
            Event::Progress { done, total } => {
                let clamped_done = if *total > 0 {
                    (*done).min(*total)
                } else {
                    *done
                };
                self.done_bytes = clamped_done;
                self.total_bytes = *total;
                self.progress = Some(format_transfer_progress(*done, *total, bundle));
                self.phase = MacSessionPhase::Transferring;
            }
        }
        self.snapshot(bundle)
    }

    /// Records a verified incoming offer on the receiver side and transitions to
    /// [`MacSessionPhase::OfferPending`].
    pub fn apply_offer(
        &mut self,
        summary: &OfferSummary,
        words: Vec<String>,
        bundle: &MacLocalizationBundle,
    ) -> MacSessionSnapshot {
        let preview = MacOfferPreview::from_offer_summary(summary, bundle);
        self.total_bytes = summary.total_size;
        self.done_bytes = 0;
        self.verification_words = words;
        self.offer = Some(preview);
        self.error = None;
        self.phase = MacSessionPhase::OfferPending;
        self.snapshot(bundle)
    }

    /// Marks the transfer completed, clamping final bytes and releasing the sleep assertion.
    pub fn mark_completed(&mut self, bundle: &MacLocalizationBundle) -> MacSessionSnapshot {
        if self.total_bytes > 0 {
            self.done_bytes = self.total_bytes;
            self.progress = Some(format_transfer_progress(
                self.total_bytes,
                self.total_bytes,
                bundle,
            ));
        }
        self.error = None;
        self.phase = MacSessionPhase::Completed;
        self.snapshot(bundle)
    }

    /// Maps `error` into a localized [`MacErrorAlert`], enters [`MacSessionPhase::Failed`],
    /// and releases the sleep assertion.
    pub fn mark_failed(
        &mut self,
        error: &CoreError,
        bundle: &MacLocalizationBundle,
    ) -> MacSessionSnapshot {
        let is_sender = matches!(self.role, MacSessionRole::Sender);
        self.error = Some(describe_core_error(error, is_sender, bundle));
        self.phase = MacSessionPhase::Failed;
        self.snapshot(bundle)
    }

    /// Cancels the [`MacStopHandle`], resets `phase` to [`MacSessionPhase::Idle`], and
    /// releases the sleep assertion.
    pub fn stop(&mut self, bundle: &MacLocalizationBundle) -> MacSessionSnapshot {
        self.stop.cancel();
        self.phase = MacSessionPhase::Idle;
        self.progress = None;
        self.snapshot(bundle)
    }

    /// Builds a deterministic [`MacSessionSnapshot`] in `bundle`'s negotiated language.
    #[must_use]
    pub fn snapshot(&self, bundle: &MacLocalizationBundle) -> MacSessionSnapshot {
        let status_headline = match self.phase {
            MacSessionPhase::Idle => match self.role {
                MacSessionRole::Sender => bundle.text("sendTitle"),
                MacSessionRole::Receiver => bundle.text("receiveTitle"),
            },
            MacSessionPhase::Preparing => bundle.text("sendPreparing"),
            MacSessionPhase::WaitingForPeer => bundle.text("sendWaiting"),
            MacSessionPhase::Connecting => bundle.text("receiveConnecting"),
            MacSessionPhase::OfferPending => self
                .offer
                .as_ref()
                .map_or_else(|| bundle.text("receiveOffer"), |o| o.headline.clone()),
            MacSessionPhase::Connected | MacSessionPhase::Transferring => match self.role {
                MacSessionRole::Sender => bundle.text("sendSending"),
                MacSessionRole::Receiver => bundle.text("receiveReceiving"),
            },
            MacSessionPhase::Completed => match self.role {
                MacSessionRole::Sender => bundle.text("sendDone"),
                MacSessionRole::Receiver => bundle.text("receiveDone"),
            },
            MacSessionPhase::Failed => self
                .error
                .as_ref()
                .map_or_else(|| bundle.text("errorOther"), |e| e.headline.clone()),
        };

        let menu_bar_status = match self.phase {
            MacSessionPhase::Idle | MacSessionPhase::Completed | MacSessionPhase::Failed => {
                "TotalDrop".to_owned()
            }
            MacSessionPhase::Preparing => bundle.text("sendPreparing"),
            MacSessionPhase::WaitingForPeer => self
                .display_code
                .clone()
                .unwrap_or_else(|| bundle.text("sendWaiting")),
            MacSessionPhase::Connecting => bundle.text("receiveConnecting"),
            MacSessionPhase::OfferPending => self
                .offer
                .as_ref()
                .map_or_else(|| bundle.text("receiveOffer"), |o| o.headline.clone()),
            MacSessionPhase::Connected => match self.role {
                MacSessionRole::Sender => bundle.text("sendSending"),
                MacSessionRole::Receiver => bundle.text("receiveReceiving"),
            },
            MacSessionPhase::Transferring => self.progress.as_ref().map_or_else(
                || status_headline.clone(),
                |prog| prog.menu_bar_label.clone(),
            ),
        };

        MacSessionSnapshot {
            role: self.role,
            phase: self.phase,
            locale: bundle.tag().to_owned(),
            is_rtl: bundle.is_rtl(),
            window_activity: self.phase.window_activity(),
            requires_sleep_assertion: self.phase.requires_sleep_assertion(),
            sleep_assertion_reason: bundle.text("keepAwakeReason"),
            menu_bar_status,
            status_headline,
            display_code: self.display_code.clone(),
            universal_link: self.universal_link.clone(),
            verification_words: self.verification_words.clone(),
            done_bytes: self.done_bytes,
            total_bytes: self.total_bytes,
            progress: self.progress.clone(),
            offer: self.offer.clone(),
            error: self.error.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use std::path::PathBuf;

    use super::*;
    use crate::{MacTransferConfig, connect_macos_receive, prepare_macos_file_send};

    fn bundle_for(tag: &str, numeric: &str) -> MacLocalizationBundle {
        MacLocalizationBundle::from_macos_preferences(&[tag.to_owned()], Some(numeric))
    }

    #[test]
    fn event_sequence_projects_waiting_connected_and_progress_flooring_and_clamping() {
        let bundle = bundle_for("en-US", "en_US");
        let code = TransferCode::parse("7-harbor-rocket").unwrap();
        let mut coord = MacSessionCoordinator::new(MacSessionRole::Sender);
        coord.set_code(&code);

        let idle = coord.snapshot(&bundle);
        assert_eq!(idle.phase, MacSessionPhase::Idle);
        assert!(!idle.requires_sleep_assertion);
        assert_eq!(idle.menu_bar_status, "TotalDrop");

        let waiting = coord.apply_event(&Event::Waiting, &bundle);
        assert_eq!(waiting.phase, MacSessionPhase::WaitingForPeer);
        assert!(waiting.requires_sleep_assertion);
        assert_eq!(waiting.window_activity, WindowActivity::TransferRunning);
        assert_eq!(waiting.menu_bar_status, "7-harbor-rocket");
        assert_eq!(
            waiting.universal_link.as_deref(),
            Some("https://totaldrop.app/r#7-harbor-rocket")
        );

        let words = vec![
            "able".to_owned(),
            "acid".to_owned(),
            "aged".to_owned(),
            "also".to_owned(),
        ];
        let connected = coord.apply_event(
            &Event::Connected {
                words: words.clone(),
            },
            &bundle,
        );
        assert_eq!(connected.phase, MacSessionPhase::Connected);
        assert!(connected.requires_sleep_assertion);
        assert_eq!(connected.verification_words, words);

        // 999_999 / 1_000_000 must floor to 99%, never prematurely report 100%.
        let almost_done = coord.apply_event(
            &Event::Progress {
                done: 999_999,
                total: 1_000_000,
            },
            &bundle,
        );
        assert_eq!(almost_done.phase, MacSessionPhase::Transferring);
        assert!(almost_done.requires_sleep_assertion);
        assert_eq!(almost_done.done_bytes, 999_999);
        assert_eq!(almost_done.total_bytes, 1_000_000);
        assert_eq!(almost_done.verification_words, words);
        let prog = almost_done.progress.as_ref().unwrap();
        assert_eq!(prog.percent_value, 99);
        assert_eq!(almost_done.menu_bar_status, prog.menu_bar_label);
        assert!(almost_done.menu_bar_status.starts_with("99%"));

        // Overshoot (> total) must clamp cleanly to total and 100%.
        let clamped = coord.apply_event(
            &Event::Progress {
                done: 1_250_000,
                total: 1_000_000,
            },
            &bundle,
        );
        assert_eq!(clamped.done_bytes, 1_000_000);
        assert_eq!(clamped.progress.as_ref().unwrap().percent_value, 100);

        let completed = coord.mark_completed(&bundle);
        assert_eq!(completed.phase, MacSessionPhase::Completed);
        assert!(!completed.requires_sleep_assertion);
        assert_eq!(completed.window_activity, WindowActivity::Idle);
        assert_eq!(completed.menu_bar_status, "TotalDrop");
    }

    #[test]
    fn offer_preview_detects_hidden_bidi_controls_even_beyond_inline_cap() {
        let bundle = bundle_for("en-US", "en_US");
        let summary = OfferSummary {
            files: 3,
            dirs: 1,
            total_size: 4_500_000,
            top_level: vec![
                "Reports".to_owned(),
                "invoice\u{202E}fdp.app".to_owned(),
                "notes\u{2066}rtl\u{2069}.txt".to_owned(),
            ],
            text: false,
        };

        let mut coord = MacSessionCoordinator::new(MacSessionRole::Receiver);
        let snap = coord.apply_offer(
            &summary,
            vec![
                "arch".to_owned(),
                "area".to_owned(),
                "army".to_owned(),
                "atom".to_owned(),
            ],
            &bundle,
        );
        assert_eq!(snap.phase, MacSessionPhase::OfferPending);
        assert!(snap.requires_sleep_assertion);
        assert_eq!(snap.window_activity, WindowActivity::OfferPending);

        let offer = snap.offer.as_ref().unwrap();
        assert!(offer.has_hidden_controls);
        assert_eq!(offer.files, 3);
        assert_eq!(offer.dirs, 1);
        assert_eq!(offer.formatted_size, "4.5 MB");
        assert_eq!(offer.top_level_names[0], "Reports");
        assert_eq!(offer.top_level_names[1], "invoice<U+202E>fdp.app");
        assert_eq!(offer.top_level_names[2], "notes<U+2066>rtl<U+2069>.txt");
        assert_eq!(offer.remaining_names, 0);
        assert!(offer.more_names_line.is_none());

        // Hidden bidi control placed at index 11 (past MAX_OFFER_PREVIEW_NAMES) is still flagged.
        let mut many_names: Vec<String> = (0..11).map(|i| format!("clean_{i}.pdf")).collect();
        many_names.push("sneaky\u{202E}txt.pkg".to_owned());
        let large_summary = OfferSummary {
            files: 12,
            dirs: 0,
            total_size: 12_000,
            top_level: many_names,
            text: false,
        };
        let large_preview = MacOfferPreview::from_offer_summary(&large_summary, &bundle);
        assert!(large_preview.has_hidden_controls);
        assert_eq!(large_preview.top_level_names.len(), MAX_OFFER_PREVIEW_NAMES);
        assert_eq!(large_preview.remaining_names, 2);
        assert_eq!(large_preview.more_names_line.as_deref(), Some("and 2 more"));
    }

    #[tokio::test]
    async fn pre_stopped_mac_stop_handle_aborts_prepare_send_and_connect_receive() {
        let cfg = MacTransferConfig::default();
        let mut coord = MacSessionCoordinator::new(MacSessionRole::Sender);
        let bundle = bundle_for("en-US", "en_US");
        let stopped_snap = coord.stop(&bundle);
        assert!(!stopped_snap.requires_sleep_assertion);
        assert!(coord.stop_handle().is_cancelled());

        let send_err = prepare_macos_file_send(
            &cfg,
            &[PathBuf::from("/nonexistent/should-not-even-be-touched.bin")],
            coord.stop_handle(),
        )
        .await
        .expect_err("pre-stopped send preparation must fail immediately");
        assert!(
            matches!(send_err, CoreError::Stopped),
            "expected CoreError::Stopped, got {send_err:?}"
        );

        let recv_err = connect_macos_receive(
            &cfg,
            "https://totaldrop.app/r#7-harbor-rocket",
            coord.stop_handle(),
        )
        .await
        .expect_err("pre-stopped receive connect must fail immediately");
        assert!(
            matches!(recv_err, CoreError::Stopped),
            "expected CoreError::Stopped, got {recv_err:?}"
        );
    }

    #[test]
    fn json_schema_is_deterministic_across_english_turkish_and_arabic_rtl_bundles() {
        let code = TransferCode::parse("9-harbor-rocket").unwrap();
        let cases = [
            ("en-US", "en_US", "en", false),
            ("tr-TR", "tr_TR", "tr", false),
            ("ar-SA", "ar_SA", "ar", true),
        ];

        for (lang_pref, numeric_pref, expected_tag, expected_rtl) in cases {
            let bundle = bundle_for(lang_pref, numeric_pref);
            let mut coord = MacSessionCoordinator::new(MacSessionRole::Receiver);
            let _ = coord.begin_connecting(&code, &bundle);
            let _ = coord.apply_offer(
                &OfferSummary {
                    files: 2,
                    dirs: 1,
                    total_size: 1_500_000,
                    top_level: vec!["Archive".to_owned(), "receipt\u{202E}fdp.dmg".to_owned()],
                    text: false,
                },
                vec![
                    "able".to_owned(),
                    "acid".to_owned(),
                    "aged".to_owned(),
                    "also".to_owned(),
                ],
                &bundle,
            );
            let snap = coord.apply_event(
                &Event::Progress {
                    done: 750_000,
                    total: 1_500_000,
                },
                &bundle,
            );

            let first_json = snap.to_json();
            let second_json = snap.to_json();
            assert_eq!(first_json, second_json);

            let parsed: Value = serde_json::from_str(&first_json).unwrap();
            let obj = parsed.as_object().unwrap();
            for required_field in [
                "role",
                "phase",
                "locale",
                "isRtl",
                "isBusy",
                "requiresSleepAssertion",
                "sleepAssertionReason",
                "menuBarStatus",
                "statusHeadline",
                "displayCode",
                "universalLink",
                "verificationWords",
                "doneBytes",
                "totalBytes",
                "progress",
                "offer",
                "error",
            ] {
                assert!(
                    obj.contains_key(required_field),
                    "missing JSON schema field `{required_field}` for locale `{expected_tag}`"
                );
            }

            assert_eq!(parsed["role"], "receiver");
            assert_eq!(parsed["phase"], "transferring");
            assert_eq!(parsed["locale"], expected_tag);
            assert_eq!(parsed["isRtl"], expected_rtl);
            assert_eq!(parsed["isBusy"], true);
            assert_eq!(parsed["requiresSleepAssertion"], true);
            assert!(!parsed["sleepAssertionReason"].as_str().unwrap().is_empty());
            assert!(!parsed["menuBarStatus"].as_str().unwrap().is_empty());
            assert_eq!(parsed["displayCode"], "9-harbor-rocket");
            assert_eq!(
                parsed["universalLink"],
                "https://totaldrop.app/r#9-harbor-rocket"
            );
            assert_eq!(parsed["doneBytes"], 750_000);
            assert_eq!(parsed["totalBytes"], 1_500_000);
            assert_eq!(parsed["progress"]["percentValue"], 50);
            assert_eq!(parsed["offer"]["files"], 2);
            assert_eq!(parsed["offer"]["dirs"], 1);
            assert_eq!(parsed["offer"]["hasHiddenControls"], true);
            assert_eq!(
                parsed["offer"]["topLevelNames"][1],
                "receipt<U+202E>fdp.dmg"
            );
            assert!(parsed["error"].is_null());
        }
    }
}
