// swift-tools-version: 5.9

import PackageDescription

let package = Package(
    name: "IMHelper",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "IMHelper", type: .dynamic, targets: ["IMHelper"]),
    ],
    targets: [
        .target(
            name: "IMHelper",
            path: "Sources/IMHelper"
        ),
        .testTarget(
            name: "IMHelperTests",
            dependencies: ["IMHelper"],
            path: "Tests/IMHelperTests"
        ),
    ]
)
