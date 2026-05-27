import XCTest
import Foundation
@testable import WindInputKit
#if canImport(Darwin)
import Darwin
#endif

/// PushClient 单测: 用 socketpair 模拟 server, push 几帧给 client, 验证 onFrame 被
/// 正确回调, stop() 优雅退出. 不依赖真实 Go 服务.
final class PushClientTests: XCTestCase {

    /// 临时 UDS server socket, 用一个 listening socket + 单 accept 协程.
    private final class FakePushServer {
        let socketPath: String
        private var listenFd: Int32 = -1
        private var clientFd: Int32 = -1
        private let acceptedSem = DispatchSemaphore(value: 0)

        init() {
            let dir = NSTemporaryDirectory()
            self.socketPath = "\(dir)wind_test_push_\(UUID().uuidString.prefix(8)).sock"
        }

        deinit { stop() }

        func start() throws {
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
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    Darwin.bind(listenFd, $0, len)
                }
            }
            XCTAssertEqual(bound, 0, "bind: \(String(cString: strerror(errno)))")
            XCTAssertEqual(Darwin.listen(listenFd, 1), 0)

            // 后台 accept (避免阻塞主线程)
            DispatchQueue.global().async { [self] in
                var caddr = sockaddr_un()
                var clen = socklen_t(MemoryLayout<sockaddr_un>.size)
                let fd = withUnsafeMutablePointer(to: &caddr) {
                    $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                        Darwin.accept(listenFd, $0, &clen)
                    }
                }
                if fd >= 0 {
                    clientFd = fd
                }
                acceptedSem.signal()
            }
        }

        /// 等 PushClient 连上来.
        func waitAccepted(timeout: TimeInterval = 2) -> Bool {
            acceptedSem.wait(timeout: .now() + timeout) == .success
        }

        /// 向已 accept 的 client push 一帧 bytes (原样 write).
        func send(_ data: Data) {
            guard clientFd >= 0 else { return }
            data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) in
                var sent = 0
                let total = data.count
                let base = raw.baseAddress!
                while sent < total {
                    let n = Darwin.write(clientFd, base.advanced(by: sent), total - sent)
                    if n <= 0 { break }
                    sent += n
                }
            }
        }

        func stop() {
            if clientFd >= 0 { Darwin.close(clientFd); clientFd = -1 }
            if listenFd >= 0 { Darwin.close(listenFd); listenFd = -1 }
            _ = unlink(socketPath)
        }
    }

    func testPushClient_ReceivesStatePushFrame() throws {
        let server = FakePushServer()
        try server.start()
        defer { server.stop() }

        let client = PushClient(socketPath: server.socketPath)
        defer { client.stop() }

        let received = NSMutableArray()
        let recvLock = NSLock()
        let frameExp = expectation(description: "got frame")

        client.onFrame = { frame in
            recvLock.lock()
            received.add(frame)
            recvLock.unlock()
            frameExp.fulfill()
        }

        try client.start()
        XCTAssertTrue(server.waitAccepted())

        // 构一个 CmdStatePush 帧: header(8) + statusHeader(12, flags+0+0) + iconLabel
        let label = "英".data(using: .utf8)!
        let payloadLen = UInt32(12 + label.count)
        var frame = BinaryCodec.encodeHeader(cmd: DownstreamCmd.statePush, payloadLen: payloadLen)
        // status header: flags(u32) + keyDownCount(u32, 0) + keyUpCount(u32, 0)
        var statusHdr = Data(count: 12)
        statusHdr.writeUInt32LE(0x0001, at: 0)  // StatusChineseMode
        statusHdr.writeUInt32LE(0, at: 4)
        statusHdr.writeUInt32LE(0, at: 8)
        frame.append(statusHdr)
        frame.append(label)
        server.send(frame)

        wait(for: [frameExp], timeout: 2.0)

        recvLock.lock()
        defer { recvLock.unlock() }
        XCTAssertEqual(received.count, 1)
        guard let f = received[0] as? Frame else {
            XCTFail("not a Frame")
            return
        }
        XCTAssertEqual(f.cmd, DownstreamCmd.statePush)
        XCTAssertEqual(f.payload.count, Int(payloadLen))
        // 验 iconLabel 在 payload 末尾
        let recoveredLabel = String(data: f.payload.suffix(label.count), encoding: .utf8)
        XCTAssertEqual(recoveredLabel, "英")
    }

    func testPushClient_StopIsIdempotent() throws {
        let server = FakePushServer()
        try server.start()
        defer { server.stop() }

        let client = PushClient(socketPath: server.socketPath)
        try client.start()
        XCTAssertTrue(server.waitAccepted())
        XCTAssertTrue(client.isRunning)

        client.stop()
        XCTAssertFalse(client.isRunning)

        client.stop()       // 第二次幂等
        XCTAssertFalse(client.isRunning)
    }

    func testPushClient_MultipleFrames() throws {
        let server = FakePushServer()
        try server.start()
        defer { server.stop() }

        let client = PushClient(socketPath: server.socketPath)
        defer { client.stop() }

        let counter = NSCountedSet()
        let lock = NSLock()
        let exp = expectation(description: "got 3 frames")
        exp.expectedFulfillmentCount = 3

        client.onFrame = { frame in
            lock.lock()
            counter.add(frame.cmd)
            lock.unlock()
            exp.fulfill()
        }

        try client.start()
        XCTAssertTrue(server.waitAccepted())

        // 推 3 帧 ServiceReady (空 payload), 共 8 字节 header × 3
        let ready = BinaryCodec.encodeHeader(cmd: DownstreamCmd.serviceReady, payloadLen: 0)
        server.send(ready)
        server.send(ready)
        server.send(ready)

        wait(for: [exp], timeout: 2.0)

        lock.lock()
        XCTAssertEqual(counter.count(for: DownstreamCmd.serviceReady), 3)
        lock.unlock()
    }
}
