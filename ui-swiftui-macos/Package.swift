// swift-tools-version: 6.0
// SPDX-FileCopyrightText: 2026 The TotalDrop Contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import PackageDescription

let package = Package(
    name: "TotalDropMac",
    defaultLocalization: "en",
    platforms: [
        .macOS(.v14),
    ],
    products: [
        .executable(
            name: "TotalDropMac",
            targets: ["TotalDropMac"]
        ),
        .library(
            name: "FinderShareExtension",
            type: .dynamic,
            targets: ["FinderShareExtension"]
        ),
    ],
    targets: [
        .executableTarget(
            name: "TotalDropMac",
            path: "Sources/TotalDropMac",
            swiftSettings: [
                .swiftLanguageMode(.v6),
            ],
            linkerSettings: [
                .linkedFramework("AppKit"),
                .linkedFramework("SwiftUI"),
                .linkedFramework("IOKit"),
                .linkedFramework("Vision"),
                .linkedFramework("CoreImage"),
                .linkedFramework("UniformTypeIdentifiers"),
            ]
        ),
        .target(
            name: "FinderShareExtension",
            path: "Sources/FinderShareExtension",
            swiftSettings: [
                .swiftLanguageMode(.v6),
            ],
            linkerSettings: [
                .linkedFramework("AppKit"),
                .linkedFramework("SwiftUI"),
                .linkedFramework("UniformTypeIdentifiers"),
            ]
        ),
        .testTarget(
            name: "TotalDropMacTests",
            dependencies: ["TotalDropMac"],
            path: "Tests/TotalDropMacTests",
            swiftSettings: [
                .swiftLanguageMode(.v6),
            ]
        ),
    ]
)
