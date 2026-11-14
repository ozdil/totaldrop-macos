// SPDX-FileCopyrightText: 2026 The TotalDrop Contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// Native macOS Finder Share Extension & Quick Action view controller (`NSExtensionPrincipalClass`).
///
/// Collects `public.file-url` attachments selected in Finder, normalizes APFS NFD file names
/// to NFC, filters case-insensitive top-level duplicates, and hands the batch to `TotalDropMac`.
@MainActor
public final class ShareViewController: NSViewController {
    private var validatedURLs: [URL] = []
    private var skippedCount: Int = 0

    override public var nibName: NSNib.Name? {
        nil
    }

    override public func loadView() {
        let container = NSHostingView(
            rootView: FinderShareSheetView(
                onSend: { [weak self] in self?.confirmAndHandOff() },
                onCancel: { [weak self] in self?.cancelExtension() }
            )
        )
        container.frame = NSRect(x: 0, y: 0, width: 420, height: 220)
        self.view = container
    }

    override public func viewDidLoad() {
        super.viewDidLoad()
        Task { @MainActor in
            await extractSharedFileURLs()
        }
    }

    private func extractSharedFileURLs() async {
        guard let items = extensionContext?.inputItems as? [NSExtensionItem] else { return }
        var collected: [URL] = []
        var seenCollisionKeys = Set<String>()
        var skipped = 0

        for item in items {
            for provider in item.attachments ?? [] {
                guard provider.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier) else {
                    skipped += 1
                    continue
                }
                if let url = await Self.loadFileURL(from: provider) {
                    let normalized = url.lastPathComponent.precomposedStringWithCanonicalMapping
                    let key = normalized.lowercased()
                    if normalized.isEmpty || seenCollisionKeys.contains(key) {
                        skipped += 1
                    } else {
                        seenCollisionKeys.insert(key)
                        collected.append(url)
                    }
                } else {
                    skipped += 1
                }
            }
        }
        self.validatedURLs = collected
        self.skippedCount = skipped
    }

    private static func loadFileURL(from provider: NSItemProvider) async -> URL? {
        await withCheckedContinuation { continuation in
            provider.loadItem(forTypeIdentifier: UTType.fileURL.identifier, options: nil) { @Sendable item, _ in
                if let url = item as? URL, url.isFileURL {
                    continuation.resume(returning: url)
                } else if let data = item as? Data,
                          let url = URL(dataRepresentation: data, relativeTo: nil),
                          url.isFileURL
                {
                    continuation.resume(returning: url)
                } else {
                    continuation.resume(returning: nil)
                }
            }
        }
    }

    private func confirmAndHandOff() {
        if let defaults = UserDefaults(suiteName: "group.app.totaldrop.mac") {
            let encoded = validatedURLs.map(\.absoluteString)
            defaults.set(encoded, forKey: "pendingFinderShareURLs")
            defaults.set(skippedCount, forKey: "pendingFinderSkippedCount")
        }
        if let launchURL = URL(string: "totaldrop://share-handoff") {
            NSWorkspace.shared.open(launchURL)
        }
        extensionContext?.completeRequest(returningItems: [], completionHandler: nil)
    }

    private func cancelExtension() {
        let cancelError = NSError(domain: NSCocoaErrorDomain, code: NSUserCancelledError)
        extensionContext?.cancelRequest(withError: cancelError)
    }
}

private struct FinderShareSheetView: View {
    let onSend: @MainActor () -> Void
    let onCancel: @MainActor () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(spacing: 12) {
                Image(systemName: "arrow.up.arrow.down.circle.fill")
                    .font(.system(size: 32))
                    .foregroundStyle(.tint)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Send with TotalDrop")
                        .font(.headline)
                    Text("End-to-end encrypted transfer from Finder")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            Text("TotalDrop will prepare the selected items and generate a one-time transfer code.")
                .font(.callout)
                .foregroundStyle(.secondary)

            Spacer()

            HStack {
                Button("Cancel", action: onCancel)
                    .keyboardShortcut(.cancelAction)
                Spacer()
                Button("Open in TotalDrop", action: onSend)
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(20)
        .background(.regularMaterial)
    }
}
