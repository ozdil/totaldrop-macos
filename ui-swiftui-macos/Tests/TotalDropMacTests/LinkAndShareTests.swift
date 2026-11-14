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
}
