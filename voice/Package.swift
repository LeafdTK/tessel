// swift-tools-version: 6.1
// tessel-voice: the microphone, and the speech engines that only exist in
// swift (apple's own, and later parakeet), for tessel's message box. it's a
// helper the app talks to over stdin and stdout; see Sources/tessel-voice/main.swift.

import PackageDescription

let package = Package(
    name: "tessel-voice",
    platforms: [.macOS("26")],
    products: [
        .executable(name: "tessel-voice", targets: ["tessel-voice"]),
    ],
    targets: [
        .executableTarget(
            name: "tessel-voice",
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
    ]
)
