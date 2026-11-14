// SPDX-FileCopyrightText: 2026 The TotalDrop Contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation
import Testing
@testable import TotalDropMac

@Suite("TotalDropMac Link & Finder Share Routing")
struct LinkAndShareTests {
    @Test("Extracts code from Universal Link fragment")
    func universalLinkFragment() throws {
        let url = try #require(URL(string: "https://totaldrop.app/r#7-harbor-rocket"))
        #expect(MacAppModel.extractTransferCode(from: url) == "7-harbor-rocket")
    }

    @Test("Extracts code from custom totaldrop scheme")
    func customScheme() throws {
        let url = try #require(URL(string: "totaldrop://r/12-harbor-rocket-meadow"))
        #expect(MacAppModel.extractTransferCode(from: url) == "12-harbor-rocket-meadow")
    }

    @Test("Rejects non-TotalDrop host")
    func untrustedHost() throws {
        let url = try #require(URL(string: "https://example.org/r#7-harbor-rocket"))
        #expect(MacAppModel.extractTransferCode(from: url) == nil)
    }

    @Test("Deduplicates case-colliding Finder file URLs")
    @MainActor
    func finderBatchCollision() {
        let model = MacAppModel()
        let urls = [
            URL(fileURLWithPath: "/Users/ada/A/Report.pdf"),
            URL(fileURLWithPath: "/Users/ada/B/report.pdf"),
        ]
        model.addSharedFileURLs(urls)
        #expect(model.selectedFiles.count == 1)
        #expect(model.transientBanner != nil)
    }

    @Test("Routes incoming URL to Receive when idle and preserves active send when busy")
    @MainActor
    func incomingURLIdleAndBusyRouting() throws {
        let idleModel = MacAppModel()
        let firstLink = try #require(URL(string: "https://totaldrop.app/r#7-harbor-rocket"))
        idleModel.handleIncomingURL(firstLink)
        #expect(idleModel.selectedTab == .receive)
        #expect(idleModel.receiveInputCode == "7-harbor-rocket")
        #expect(idleModel.phase == .connecting(code: "7-harbor-rocket"))
        #expect(idleModel.menuBarStatusTitle == "Connecting…")
        #expect(idleModel.transientBanner == nil)
        idleModel.stopActiveTransfer()

        let busyModel = MacAppModel()
        busyModel.beginSend()
        let expectedSendPhase = MacTransferPhase.waitingForReceiver(
            code: "7-harbor-rocket",
            universalLink: "https://totaldrop.app/r#7-harbor-rocket"
        )
        #expect(busyModel.phase.isBusy)
        #expect(busyModel.phase == expectedSendPhase)

        let competingLink = try #require(URL(string: "totaldrop://r/9-harbor-rocket"))
        busyModel.handleIncomingURL(competingLink)
        #expect(busyModel.selectedTab == .send)
        #expect(busyModel.receiveInputCode.isEmpty)
        #expect(busyModel.phase == expectedSendPhase)
        #expect(busyModel.transientBanner == "Finish or stop what is on the screen, then open the link again.")
        busyModel.stopActiveTransfer()
    }

    @Test("Generates 2-word and 3-word waiting codes, updates menuBarStatusTitle, and resets on stop")
    @MainActor
    func beginSendCodeStrengthAndStopLifecycle() {
        let model = MacAppModel()
        #expect(model.phase == .idle)
        #expect(model.menuBarStatusTitle == "TotalDrop")

        model.strongerCode = false
        model.beginSend()
        #expect(
            model.phase == .waitingForReceiver(
                code: "7-harbor-rocket",
                universalLink: "https://totaldrop.app/r#7-harbor-rocket"
            )
        )
        #expect(model.menuBarStatusTitle == "7-harbor-rocket")

        model.stopActiveTransfer()
        #expect(model.phase == .idle)
        #expect(!model.phase.isBusy)
        #expect(model.menuBarStatusTitle == "TotalDrop")

        model.strongerCode = true
        model.beginSend()
        #expect(
            model.phase == .waitingForReceiver(
                code: "7-harbor-rocket-meadow",
                universalLink: "https://totaldrop.app/r#7-harbor-rocket-meadow"
            )
        )
        #expect(model.menuBarStatusTitle == "7-harbor-rocket-meadow")

        model.stopActiveTransfer()
        #expect(model.phase == .idle)
        #expect(model.menuBarStatusTitle == "TotalDrop")
    }

    @Test("Rejects non-file HTTPS URLs in addSharedFileURLs and updates skipped banner")
    @MainActor
    func rejectsNonFileURLsInSharedBatch() throws {
        let singleModel = MacAppModel()
        let remoteOne = try #require(URL(string: "https://totaldrop.app/r#7-harbor-rocket"))
        singleModel.addSharedFileURLs([remoteOne])
        #expect(singleModel.selectedFiles.isEmpty)
        #expect(
            singleModel.transientBanner
                == "One shared item could not be read or collided by name and was left out."
        )

        let mixedModel = MacAppModel()
        let remoteTwo = try #require(URL(string: "https://example.org/archive.zip"))
        let localFile = URL(fileURLWithPath: "/Users/ada/Documents/Notes.txt")
        mixedModel.addSharedFileURLs([remoteOne, remoteTwo, localFile])
        #expect(mixedModel.selectedFiles == [localFile])
        #expect(
            mixedModel.transientBanner
                == "2 shared items could not be read or collided by name and were left out."
        )
    }
}
