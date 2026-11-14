// SPDX-FileCopyrightText: 2026 The TotalDrop Contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import SwiftUI
import UniformTypeIdentifiers
import Vision

/// Native macOS SwiftUI view for entering or pasting a transfer code, scanning a QR image,
/// reviewing the offer and verification words, and saving files to `~/Downloads/TotalDrop`.
public struct ReceiveView: View {
    @Bindable var model: MacAppModel
    @State private var isQRImagePickerPresented = false

    public init(model: MacAppModel) {
        self.model = model
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Receive files")
                        .font(.title2.weight(.semibold))
                    Text("Enter a code or open a TotalDrop link to inspect and save verified files.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }

                switch model.phase {
                case .connecting(let code):
                    connectingCard(code: code)
                case .offerPending(let files, let totalBytes, let words, let names, let hasHiddenControls):
                    offerCard(files: files, totalBytes: totalBytes, words: words, names: names, hasHiddenControls: hasHiddenControls)
                case .transferring(_, _, let percent, let fractionText, _, let words):
                    receivingCard(percent: percent, fractionText: fractionText, words: words)
                case .completed(let summaryText, let savedFolder):
                    completedCard(summaryText: summaryText, savedFolder: savedFolder)
                case .failed(let headline, let cause, let detail):
                    failedCard(headline: headline, cause: cause, detail: detail)
                default:
                    codeEntryCard
                }
            }
            .padding(24)
        }
        .fileImporter(
            isPresented: $isQRImagePickerPresented,
            allowedContentTypes: [.image],
            allowsMultipleSelection: false
        ) { result in
            if case .success(let urls), let imageURL = urls.first {
                readQRCode(from: imageURL)
            }
        }
    }

    private var codeEntryCard: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Code")
                .font(.headline)

            HStack(spacing: 10) {
                TextField("7-harbor-rocket", text: $model.receiveInputCode)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(.body, design: .monospaced))
                    .onSubmit {
                        model.startReceiveConnection()
                    }

                Button {
                    if let pasted = NSPasteboard.general.string(forType: .string) {
                        if let url = URL(string: pasted.trimmingCharacters(in: .whitespacesAndNewlines)),
                           let extracted = MacAppModel.extractTransferCode(from: url)
                        {
                            model.receiveInputCode = extracted
                        } else {
                            model.receiveInputCode = pasted.trimmingCharacters(in: .whitespacesAndNewlines)
                        }
                    }
                } label: {
                    Label("Paste", systemImage: "doc.on.clipboard")
                }

                Button {
                    isQRImagePickerPresented = true
                } label: {
                    Label("Read QR from image…", systemImage: "qrcode.viewfinder")
                }
            }

            HStack {
                Text("Files save to ~/Downloads/TotalDrop after BLAKE3 verification.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
                Button {
                    model.startReceiveConnection()
                } label: {
                    Text("Connect")
                        .frame(minWidth: 110)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .disabled(model.receiveInputCode.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(18)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
    }

    private func connectingCard(code: String) -> some View {
        HStack(spacing: 12) {
            ProgressView()
            VStack(alignment: .leading, spacing: 2) {
                Text("Connecting securely…")
                    .font(.headline)
                Text(code)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button("Cancel", role: .destructive) {
                model.stopActiveTransfer()
            }
        }
        .padding(18)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
    }

    private func offerCard(
        files: Int,
        totalBytes: UInt64,
        words: [String],
        names: [String],
        hasHiddenControls: Bool
    ) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Incoming files")
                .font(.title3.weight(.semibold))

            Text("\(files) file(s) · \(ByteCountFormatter.string(fromByteCount: Int64(clamping: totalBytes), countStyle: .file))")
                .font(.subheadline)
                .foregroundStyle(.secondary)

            VStack(alignment: .leading, spacing: 4) {
                Text("Verification words")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
                Text(words.joined(separator: " · "))
                    .font(.headline.monospaced())
                Text("The other person sees the same four words. If they differ, stop.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 10))

            if hasHiddenControls {
                Label("Hidden direction or control characters in names are shown as <U+…>.", systemImage: "exclamationmark.shield")
                    .font(.caption)
                    .foregroundStyle(.orange)
            }

            VStack(alignment: .leading, spacing: 4) {
                ForEach(names.prefix(10), id: \.self) { item in
                    Text("• \(item)")
                        .font(.callout.monospaced())
                }
            }

            HStack {
                Button("Decline", role: .destructive) {
                    model.stopActiveTransfer()
                }
                Spacer()
                Button("Accept") {
                    let downloads = FileManager.default.urls(for: .downloadsDirectory, in: .userDomainMask).first?
                        .appendingPathComponent("TotalDrop", isDirectory: true)
                    model.phase = .completed(summaryText: "All \(files) file(s) arrived intact.", savedFolder: downloads)
                }
                .buttonStyle(.borderedProminent)
            }
        }
        .padding(18)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
    }

    private func receivingCard(percent: Int, fractionText: String, words: [String]) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            if !words.isEmpty {
                Text(words.joined(separator: " · "))
                    .font(.subheadline.monospaced())
                    .foregroundStyle(.secondary)
            }
            ProgressView(value: Double(percent), total: 100.0) {
                HStack {
                    Text("Receiving")
                    Spacer()
                    Text("\(percent)% · \(fractionText)")
                        .monospacedDigit()
                        .foregroundStyle(.secondary)
                }
            }
            Button("Stop", role: .destructive) {
                model.stopActiveTransfer()
            }
        }
        .padding(18)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
    }

    private func completedCard(summaryText: String, savedFolder: URL?) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Label("Received and verified", systemImage: "checkmark.seal.fill")
                .font(.title3.weight(.semibold))
                .foregroundStyle(.green)
            Text(summaryText)
                .foregroundStyle(.secondary)
            HStack {
                if let folder = savedFolder {
                    Button("Open folder") {
                        NSWorkspace.shared.activateFileViewerSelecting([folder])
                    }
                }
                Button("Done") {
                    model.receiveInputCode = ""
                    model.resetCompletedOrFailed()
                }
                .buttonStyle(.borderedProminent)
            }
        }
        .padding(18)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
    }

    private func failedCard(headline: String, cause: String?, detail: String) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Label(headline, systemImage: "exclamationmark.triangle.fill")
                .font(.headline)
                .foregroundStyle(.red)
            if let cause {
                Text(cause)
                    .font(.callout)
            }
            DisclosureGroup("Details") {
                Text(detail)
                    .font(.caption.monospaced())
                    .textSelection(.enabled)
            }
            Button("Try again") {
                model.resetCompletedOrFailed()
            }
        }
        .padding(18)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
    }

    private func readQRCode(from imageURL: URL) {
        let request = VNDetectBarcodesRequest()
        request.symbologies = [.qr]
        let handler = VNImageRequestHandler(url: imageURL, options: [:])
        do {
            try handler.perform([request])
            guard let payload = request.results?.compactMap(\.payloadStringValue).first else {
                model.transientBanner = "There is no TotalDrop code in that image."
                return
            }
            if let url = URL(string: payload), let code = MacAppModel.extractTransferCode(from: url) {
                model.receiveInputCode = code
                model.startReceiveConnection()
            } else {
                model.receiveInputCode = payload
            }
        } catch {
            model.transientBanner = "There is no TotalDrop code in that image."
        }
    }
}
