// swift-tools-version:5.9
//
// WindInput macOS — SwiftPM 工程骨架 (PR-A M1)
//
// 目标:
//   - WindInputKit  : 二进制协议 codec + UDS BridgeClient (后续 IMKit .app 共用)
//   - WindInputSmoke: 命令行 smoke 工具, 连真实 bridge.sock 收发帧, 印 cmd id/len
//
// 后续 M2+ 会追加 .app target (用 xcodebuild 或 swift package generate-xcodeproj
// 后手补 Info.plist), 此时把 WindInputKit 作为依赖即可。
import PackageDescription

let package = Package(
    name: "WindInput",
    platforms: [.macOS(.v12)],
    products: [
        .library(name: "WindInputKit", targets: ["WindInputKit"]),
        .executable(name: "wind-smoke", targets: ["WindInputSmoke"]),
    ],
    targets: [
        .target(
            name: "WindInputKit",
            path: "Sources/WindInputKit"
        ),
        .executableTarget(
            name: "WindInputSmoke",
            dependencies: ["WindInputKit"],
            path: "Sources/WindInputSmoke"
        ),
        .testTarget(
            name: "WindInputKitTests",
            dependencies: ["WindInputKit"],
            path: "Tests/WindInputKitTests"
        ),
    ]
)
