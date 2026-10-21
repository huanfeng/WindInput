import XCTest
import Foundation
@testable import WindInputKit
#if canImport(Darwin)
import Darwin
#endif

/// BridgeClient 超时与冷却: 服务 listen 但从不 accept/回包 (卡死) 时, 同步请求不能无限阻塞。
final class BridgeClientTests: XCTestCase {

    private var socketPath = ""
    private var listenFd: Int32 = -1

    override func setUpWithError() throws {
        BridgeClient.resetStall()
        socketPath = "\(NSTemporaryDirectory())wind_test_bridge_\(UUID().uuidString.prefix(8)).sock"
        _ = unlink(socketPath)
        listenFd = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        XCTAssertGreaterThanOrEqual(listenFd, 0)
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let pathBytes = socketPath.utf8CString
        withUnsafeMutablePointer(to: &addr.sun_path) { rawPtr in
            rawPtr.withMemoryRebound(to: CChar.self, capacity: pathBytes.count) { dst in
                _ = pathBytes.withUnsafeBufferPointer { src in
                    memcpy(dst, src.baseAddress, src.count)
                }
            }
        }
        let len = socklen_t(MemoryLayout<sockaddr_un>.size)
        let bound = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { Darwin.bind(listenFd, $0, len) }
        }
        XCTAssertEqual(bound, 0)
        // 只 listen 不 accept: connect 落进 backlog 成功, 请求永远等不到回包。
        XCTAssertEqual(Darwin.listen(listenFd, 8), 0)
    }

    override func tearDown() {
        BridgeClient.resetStall()
        if listenFd >= 0 { Darwin.close(listenFd) }
        _ = unlink(socketPath)
    }

    private func ping() -> Data { BinaryCodec.encodeEmptyFrame(cmd: UpstreamCmd.focusGained) }

    func testReadTimesOutAndClosesConnection() throws {
        let c = try BridgeClient(socketPath: socketPath, ioTimeoutMs: 200)
        try c.send(ping())
        let start = Date()
        XCTAssertThrowsError(try c.readFrame()) { XCTAssertEqual($0 as? IPCError, .timeout) }
        XCTAssertLessThan(Date().timeIntervalSince(start), 1.5)
        XCTAssertFalse(c.isConnected, "超时后连接须关闭, 否则迟到的回包会错位")
    }

    func testCooldownFailsFastOnOtherConnections() throws {
        let first = try BridgeClient(socketPath: socketPath, ioTimeoutMs: 200)
        try first.send(ping())
        XCTAssertThrowsError(try first.readFrame())

        let second = try BridgeClient(socketPath: socketPath, ioTimeoutMs: 200)
        let start = Date()
        XCTAssertThrowsError(try second.send(ping())) { XCTAssertEqual($0 as? IPCError, .timeout) }
        XCTAssertLessThan(Date().timeIntervalSince(start), 0.1, "冷却期内不应再等满超时")
        XCTAssertTrue(second.isConnected, "冷却期内未发出请求, 连接仍同步可用")
    }

    func testCooldownDoesNotAffectPushConnections() throws {
        let req = try BridgeClient(socketPath: socketPath, ioTimeoutMs: 200)
        try req.send(ping())
        XCTAssertThrowsError(try req.readFrame())

        let push = try BridgeClient(socketPath: socketPath)
        XCTAssertNoThrow(try push.send(ping()))
    }
}
