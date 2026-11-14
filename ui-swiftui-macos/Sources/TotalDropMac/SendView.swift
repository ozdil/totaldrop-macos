// SPDX-FileCopyrightText: 2026 The TotalDrop Contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import CoreImage.CIFilterBuiltins
import SwiftUI
import UniformTypeIdentifiers

/// Native macOS SwiftUI view for picking files or text, generating a transfer code,
/// sharing Universal Links / QR codes, verifying the 4 words, and tracking progress.
public struct SendView: View {
    @Bindable var model: MacAppModel
    @State private var isDropTargeted = false
    @State private var isFileImporterPresented = false

    public init(model: MacAppModel) {
        self.model = model
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                headerRow

                switch model.phase {
                case .waitingForReceiver(let code, let universalLink):
                    waitingCard(code: code, universalLink: universalLink)
                case .transferring(_, _, let percent, let fractionText, _, let words):
                    transferringCard(percent: percent, fractionText: fractionText, words: words)
                case .completed(let summaryText, _):
                    completedCard(summaryText: summaryText)
                case .failed(let headline, let cause, let detail):
                    failedCard(headline: headline, cause: cause, detail: detail)
                default:
                    selectionCard
                }
            }
            .padding(24)
        }
        .dropDestination(for: URL.self) { items, _ in
            model.addSharedFileURLs(items)
            return true
        } isTargeted: { targeted in
            isDropTargeted = targeted
        }
        .fileImporter(
            isPresented: $isFileImporterPresented,
            allowedContentTypes: [.item, .folder],
            allowsMultipleSelection: true
        ) { result in
            if case .success(let urls) = result {
                model.addSharedFileURLs(urls)
            }
        }
    }

    private var headerRow: some View {
        HStack {
            VStack(alignment: .leading, spacing: 4) {
                Text(model.sendTextMode ? "Send text" : "Send files")
                    .font(.title2.weight(.semibold))
                Text("End-to-end encrypted with CPace, Noise XXpsk3, ML-KEM-768, and BLAKE3.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Picker("Mode", selection: $model.sendTextMode) {
                Text("Files").tag(false)
                Text("Text").tag(true)
            }
            .pickerStyle(.segmented)
            .frame(width: 170)
            .disabled(model.phase.isBusy)
        }
    }

    private var selectionCard: some View {
        VStack(alignment: .leading, spacing: 16) {
            if model.sendTextMode {
                TextEditor(text: $model.textPayload)
                    .font(.body.monospaced())
                    .frame(minHeight: 160)
                    .padding(8)
                    .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 10))
            } else {
                dropZone
                if !model.selectedFiles.isEmpty {
                    fileList
                }
            }

            Toggle(isOn: $model.strongerCode) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Stronger code")
                    Text("Three words instead of two: much harder to guess")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            HStack {
                if !model.sendTextMode {
                    Button {
                        isFileImporterPresented = true
                    } label: {
                        Label("Add files…", systemImage: "plus")
                    }
                }
                Spacer()
                Button {
                    model.beginSend()
                } label: {
                    Text("Create code")
                        .frame(minWidth: 120)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .disabled(model.sendTextMode ? model.textPayload.isEmpty : model.selectedFiles.isEmpty)
            }
        }
    }

    private var dropZone: some View {
        VStack(spacing: 10) {
            Image(systemName: "arrow.up.doc.on.clipboard")
                .font(.system(size: 30))
                .foregroundStyle(.secondary)
            Text(model.selectedFiles.isEmpty ? "Nothing selected yet" : "\(model.selectedFiles.count) item(s) ready")
                .font(.headline)
            Text("Drop files or folders here, or use Finder's Share → Send with TotalDrop.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, minHeight: 130)
        .padding(16)
        .background(
            RoundedRectangle(cornerRadius: 12)
                .fill(.regularMaterial)
                .overlay(
                    RoundedRectangle(cornerRadius: 12)
                        .strokeBorder(isDropTargeted ? Color.accentColor : Color.secondary.opacity(0.25), style: StrokeStyle(lineWidth: isDropTargeted ? 2 : 1, dash: [6]))
                )
        )
    }

    private var fileList: some View {
        VStack(spacing: 6) {
            ForEach(model.selectedFiles, id: \.self) { url in
                HStack {
                    Image(nsImage: NSWorkspace.shared.icon(forFile: url.path))
                        .resizable()
                        .frame(width: 20, height: 20)
                    Text(url.lastPathComponent.precomposedStringWithCanonicalMapping)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer()
                    Button(role: .destructive) {
                        model.selectedFiles.removeAll { $0 == url }
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .foregroundStyle(.secondary)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Remove \(url.lastPathComponent)")
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 6)
                .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 8))
            }
        }
    }

    private func waitingCard(code: String, universalLink: String) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Your code")
                .font(.subheadline)
                .foregroundStyle(.secondary)

            HStack(alignment: .center, spacing: 20) {
                VStack(alignment: .leading, spacing: 10) {
                    Text(code)
                        .font(.system(size: 28, weight: .semibold, design: .monospaced))
                        .textSelection(.enabled)

                    Text("On the other device, open TotalDrop, choose Receive, and enter this code.")
                        .font(.callout)
                        .foregroundStyle(.secondary)

                    HStack(spacing: 10) {
                        Button {
                            NSPasteboard.general.clearContents()
                            NSPasteboard.general.setString(code, forType: .string)
                        } label: {
                            Label("Copy", systemImage: "doc.on.doc")
                        }

                        if let shareURL = URL(string: universalLink) {
                            ShareLink(
                                item: shareURL,
                                message: Text("Get my files with TotalDrop. Code: \(code)")
                            ) {
                                Label("Share", systemImage: "square.and.arrow.up")
                            }
                        }

                        Button("Cancel", role: .destructive) {
                            model.stopActiveTransfer()
                        }
                    }
                }
                Spacer()
                if let qrImage = Self.makeQRCodeImage(from: universalLink) {
                    Image(nsImage: qrImage)
                        .interpolation(.none)
                        .resizable()
                        .frame(width: 132, height: 132)
                        .padding(8)
                        .background(.white, in: RoundedRectangle(cornerRadius: 10))
                        .accessibilityLabel("QR code for \(code)")
                }
            }
            .padding(18)
            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))

            HStack(spacing: 8) {
                ProgressView()
                    .controlSize(.small)
                Text("Waiting for the receiver…")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private func transferringCard(percent: Int, fractionText: String, words: [String]) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            if !words.isEmpty {
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
            }

            ProgressView(value: Double(percent), total: 100.0) {
                HStack {
                    Text("Sending")
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

    private func completedCard(summaryText: String) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Label("Sent and verified", systemImage: "checkmark.seal.fill")
                .font(.title3.weight(.semibold))
                .foregroundStyle(.green)
            Text(summaryText)
                .foregroundStyle(.secondary)
            Button("Done") {
                model.selectedFiles.removeAll()
                model.textPayload = ""
                model.resetCompletedOrFailed()
            }
            .buttonStyle(.borderedProminent)
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

    static func makeQRCodeImage(from payload: String) -> NSImage? {
        let context = CIContext()
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(payload.utf8)
        filter.correctionLevel = "M"
        guard let output = filter.outputImage else { return nil }
        let scaled = output.transformed(by: CGAffineTransform(scaleX: 6, y: 6))
        guard let cgImage = context.createCGImage(scaled, from: scaled.extent) else { return nil }
        return NSImage(cgImage: cgImage, size: NSSize(width: scaled.extent.width, height: scaled.extent.height))
    }
}
