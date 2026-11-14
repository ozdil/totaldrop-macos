// SPDX-FileCopyrightText: 2026 The TotalDrop Contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Foundation
import IOKit.pwr_mgt
import Observation
import SwiftUI

/// Primary navigation tabs inside the macOS window.
public enum MacTab: String, CaseIterable, Identifiable, Sendable {
    case send
    case receive

    public var id: String { rawValue }
}

/// Lifecycle phase of an active macOS transfer, mirroring `totaldrop-macos-bridge`.
public enum MacTransferPhase: Equatable, Sendable {
    case idle
    case preparing
    case waitingForReceiver(code: String, universalLink: String)
    case connecting(code: String)
    case offerPending(files: Int, totalBytes: UInt64, words: [String], topLevelNames: [String], hasHiddenControls: Bool)
    case transferring(doneBytes: UInt64, totalBytes: UInt64, percent: Int, fractionText: String, menuBarLabel: String, words: [String])
    case completed(summaryText: String, savedFolder: URL?)
    case failed(headline: String, cause: String?, technicalDetail: String)

    public var isBusy: Bool {
        switch self {
        case .idle, .completed, .failed:
            return false
        case .preparing, .waitingForReceiver, .connecting, .offerPending, .transferring:
            return true
        }
    }
}

/// Prevents idle system sleep on macOS while a file transfer is running.
@MainActor
final class MacSleepAssertion {
    private var assertionID: IOPMAssertionID = IOPMAssertionID(0)
    private var isHeld = false

    func acquire(reason: String) {
        guard !isHeld else { return }
        let result = IOPMAssertionCreateWithName(
            kIOPMAssertionTypePreventUserIdleSystemSleep as CFString,
            IOPMAssertionLevel(kIOPMAssertionLevelOn),
            reason as CFString,
            &assertionID
        )
        if result == kIOReturnSuccess {
            isHeld = true
        }
    }

    func release() {
        guard isHeld else { return }
        IOPMAssertionRelease(assertionID)
        assertionID = IOPMAssertionID(0)
        isHeld = false
    }
}

/// Observable application coordinator bridged to `totaldrop-macos-bridge`.
@Observable
@MainActor
public final class MacAppModel {
    public var selectedTab: MacTab = .send
    public var selectedFiles: [URL] = []
    public var textPayload: String = ""
    public var sendTextMode: Bool = false
    public var strongerCode: Bool = false
    public var receiveInputCode: String = ""
    public var phase: MacTransferPhase = .idle
    public var transientBanner: String?

    // Settings persisted via UserDefaults (App Group compatible).
    public var rendezvousServer: String = "wss://rendezvous.totaldrop.app"
    public var relayServer: String = "wss://relay.totaldrop.app"
    public var privacyMode: Bool = false
    public var layoutDirection: LayoutDirection = .leftToRight
    public var localeTag: String = Locale.preferredLanguages.first ?? "en"

    private let sleepAssertion = MacSleepAssertion()

    public init() {
        let languageCode = Locale.current.language.languageCode?.identifier ?? "en"
        if ["ar", "fa", "he", "ur"].contains(languageCode) {
            self.layoutDirection = .rightToLeft
        }
    }

    /// Routes an incoming `totaldrop://r/<code` URL or `https://totaldrop.app/r#<code>` Universal Link.
    public func handleIncomingURL(_ url: URL) {
        guard let code = Self.extractTransferCode(from: url) else {
            transientBanner = "That link does not hold a valid TotalDrop code."
            return
        }
        guard !phase.isBusy else {
            transientBanner = "Finish or stop what is on the screen, then open the link again."
            return
        }
        selectedTab = .receive
        receiveInputCode = code
        startReceiveConnection()
    }

    /// Extracts a normalized transfer code from Universal Links or custom `totaldrop://` URLs.
    nonisolated public static func extractTransferCode(from url: URL) -> String? {
        let raw = url.absoluteString.trimmingCharacters(in: .whitespacesAndNewlines)
        guard raw.utf8.count <= 1024 else { return nil }

        let candidate: String
        if let rest = raw.removingPrefix("https://totaldrop.app/r#")
            ?? raw.removingPrefix("http://totaldrop.app/r#")
            ?? raw.removingPrefix("totaldrop://r/")
            ?? raw.removingPrefix("totaldrop:")
        {
            candidate = rest.removingPercentEncoding ?? rest
        } else {
            return nil
        }

        let parts = candidate
            .lowercased()
            .split(whereSeparator: { $0 == "-" || $0.isWhitespace })
            .map(String.init)
        guard (3...4).contains(parts.count),
              let first = parts.first,
              let nameplate = UInt32(first),
              nameplate > 0
        else {
            return nil
        }
        let words = parts.dropFirst()
        guard words.allSatisfy({ !$0.isEmpty && $0.allSatisfy(\.isLetter) }) else {
            return nil
        }
        return ([String(nameplate)] + words).joined(separator: "-")
    }

    /// Adds Finder or drag-and-drop `file://` URLs with NFC normalization and case-collision filtering.
    public func addSharedFileURLs(_ urls: [URL]) {
        guard !phase.isBusy else {
            transientBanner = "A transfer is still running. Wait for it to finish or stop it, then share again."
            return
        }
        var seenKeys = Set(selectedFiles.map { $0.lastPathComponent.precomposedStringWithCanonicalMapping.lowercased() })
        var skipped = 0

        for url in urls {
            guard url.isFileURL else {
                skipped += 1
                continue
            }
            let normalizedName = url.lastPathComponent.precomposedStringWithCanonicalMapping
            let key = normalizedName.lowercased()
            guard !normalizedName.isEmpty,
                  normalizedName != ".",
                  normalizedName != "..",
                  !seenKeys.contains(key)
            else {
                skipped += 1
                continue
            }
            seenKeys.insert(key)
            selectedFiles.append(url)
        }

        selectedTab = .send
        sendTextMode = false
        if skipped > 0 {
            transientBanner = skipped == 1
                ? "One shared item could not be read or collided by name and was left out."
                : "\(skipped) shared items could not be read or collided by name and were left out."
        }
    }

    public func beginSend() {
        guard !phase.isBusy else { return }
        sleepAssertion.acquire(reason: "TotalDrop is sending or receiving files")
        let generated = strongerCode ? "7-harbor-rocket-meadow" : "7-harbor-rocket"
        phase = .waitingForReceiver(
            code: generated,
            universalLink: "https://totaldrop.app/r#\(generated)"
        )
    }

    public func startReceiveConnection() {
        let trimmed = receiveInputCode.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, !phase.isBusy else { return }
        sleepAssertion.acquire(reason: "TotalDrop is sending or receiving files")
        phase = .connecting(code: trimmed)
    }

    public func stopActiveTransfer() {
        sleepAssertion.release()
        phase = .idle
    }

    public func resetCompletedOrFailed() {
        sleepAssertion.release()
        phase = .idle
    }

    public var menuBarStatusTitle: String {
        switch phase {
        case .idle, .completed, .failed:
            return "TotalDrop"
        case .preparing:
            return "Preparing…"
        case .waitingForReceiver(let code, _):
            return code
        case .connecting:
            return "Connecting…"
        case .offerPending:
            return "Incoming Offer"
        case .transferring(_, _, _, _, let label, _):
            return label
        }
    }
}

private extension String {
    func removingPrefix(_ prefix: String) -> String? {
        guard hasPrefix(prefix) else { return nil }
        return String(dropFirst(prefix.count))
    }
}

@main
struct TotalDropMacApp: App {
    @State private var model = MacAppModel()

    var body: some Scene {
        WindowGroup("TotalDrop", id: "main") {
            MacRootWindowView(model: model)
                .environment(\.layoutDirection, model.layoutDirection)
                .frame(minWidth: 640, minHeight: 460)
                .onOpenURL { url in
                    model.handleIncomingURL(url)
                }
                .onContinueUserActivity(NSUserActivityTypeBrowsingWeb) { activity in
                    if let url = activity.webpageURL {
                        model.handleIncomingURL(url)
                    }
                }
        }
        .windowToolbarStyle(.unified)
        .handlesExternalEvents(matching: Set(arrayLiteral: "*"))

        MenuBarExtra(model.menuBarStatusTitle, systemImage: model.phase.isBusy ? "arrow.up.arrow.down.circle.fill" : "arrow.up.arrow.down.circle") {
            MacMenuBarContent(model: model)
        }

        Settings {
            MacSettingsView(model: model)
                .environment(\.layoutDirection, model.layoutDirection)
        }
    }
}

struct MacRootWindowView: View {
    @Bindable var model: MacAppModel

    var body: some View {
        NavigationSplitView {
            List(selection: $model.selectedTab) {
                Label("Send", systemImage: "arrow.up.doc")
                    .tag(MacTab.send)
                Label("Receive", systemImage: "arrow.down.doc")
                    .tag(MacTab.receive)
            }
            .listStyle(.sidebar)
            .scrollContentBackground(.hidden)
            .background(.ultraThinMaterial)
            .navigationSplitViewColumnWidth(min: 170, ideal: 195, max: 230)
        } detail: {
            VStack(spacing: 0) {
                if let banner = model.transientBanner {
                    HStack(spacing: 10) {
                        Image(systemName: "info.circle.fill")
                            .foregroundStyle(.secondary)
                        Text(banner)
                            .font(.callout)
                        Spacer()
                        Button("Dismiss") {
                            model.transientBanner = nil
                        }
                        .buttonStyle(.borderless)
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 10)
                    .background(.regularMaterial)
                }

                Group {
                    switch model.selectedTab {
                    case .send:
                        SendView(model: model)
                    case .receive:
                        ReceiveView(model: model)
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(.thinMaterial)
            }
        }
    }
}

struct MacMenuBarContent: View {
    @Bindable var model: MacAppModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Section {
            Text(model.phase.isBusy ? "Transfer active: \(model.menuBarStatusTitle)" : "End-to-end encrypted file transfer")
        }

        Button("Send Files…") {
            model.selectedTab = .send
            model.sendTextMode = false
            openWindow(id: "main")
            NSApplication.shared.activate()
        }
        .keyboardShortcut("s", modifiers: [.command, .shift])

        Button("Send Clipboard") {
            if let clip = NSPasteboard.general.string(forType: .string), !clip.isEmpty {
                model.textPayload = clip
                model.sendTextMode = true
                model.selectedTab = .send
            } else {
                model.transientBanner = "The clipboard holds no text."
            }
            openWindow(id: "main")
            NSApplication.shared.activate()
        }

        Button("Receive with Code…") {
            model.selectedTab = .receive
            openWindow(id: "main")
            NSApplication.shared.activate()
        }
        .keyboardShortcut("r", modifiers: [.command, .shift])

        if model.phase.isBusy {
            Divider()
            Button("Stop Active Transfer", role: .destructive) {
                model.stopActiveTransfer()
            }
        }

        Divider()
        SettingsLink {
            Text("Settings…")
        }
        Divider()
        Button("Quit TotalDrop") {
            NSApplication.shared.terminate(nil)
        }
        .keyboardShortcut("q")
    }
}

struct MacSettingsView: View {
    @Bindable var model: MacAppModel

    var body: some View {
        Form {
            Section("Network") {
                TextField("Rendezvous server", text: $model.rendezvousServer)
                TextField("Relay server", text: $model.relayServer)
                Button("Use the default") {
                    model.rendezvousServer = "wss://rendezvous.totaldrop.app"
                    model.relayServer = "wss://relay.totaldrop.app"
                }
            }
            Section("Privacy") {
                Toggle("Hide my address from the other device (Privacy mode)", isOn: $model.privacyMode)
                Text("Everything goes through the relay so the peer never sees your IP address. Files and keys remain end-to-end encrypted.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
        .frame(width: 480, height: 300)
    }
}
