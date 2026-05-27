import Foundation
import Darwin

// BridgeClient — Unix Domain Socket 客户端, 连接 Go 服务的 bridge.sock 或 bridge_push.sock.
//
// 设计意图 (PR-A M1):
//   - 阻塞式 read/write, 用 readFrame() 拉一帧 (header + payload), 错误抛 IPCError
//   - 不做 reconnect, 不做异步 callback —— 让 smoke CLI 和单测先跑通协议
//   - 后续 M2 在 InputController 里包 GCD queue + onCommand callback
//
// 用法:
//   let c = try BridgeClient(socketPath: BridgeEndpoints.requestSocket)
//   try c.send(BinaryCodec.encodeKeyEventFrame(...))
//   let frame = try c.readFrame()
//   c.close()
public final class BridgeClient {

    private var fd: Int32 = -1
    public let socketPath: String

    public init(socketPath: String) throws {
        self.socketPath = socketPath
        try connect()
    }

    deinit {
        close()
    }

    // MARK: - Connect

    private func connect() throws {
        let s = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard s >= 0 else {
            throw IPCError.connectFailed("socket(): \(String(cString: strerror(errno)))")
        }

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)

        // 路径长度校验 (sun_path 是 104 字节, 留 1 字节给 NUL)
        let pathBytes = socketPath.utf8CString
        guard pathBytes.count <= MemoryLayout.size(ofValue: addr.sun_path) else {
            Darwin.close(s)
            throw IPCError.connectFailed("socket path too long: \(socketPath)")
        }
        withUnsafeMutablePointer(to: &addr.sun_path) { rawPtr in
            rawPtr.withMemoryRebound(to: CChar.self, capacity: pathBytes.count) { dst in
                _ = pathBytes.withUnsafeBufferPointer { src in
                    memcpy(dst, src.baseAddress, src.count)
                }
            }
        }

        let len = socklen_t(MemoryLayout<sockaddr_un>.size)
        let rc = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(s, $0, len)
            }
        }
        guard rc == 0 else {
            let msg = "connect(\(socketPath)): \(String(cString: strerror(errno)))"
            Darwin.close(s)
            throw IPCError.connectFailed(msg)
        }

        self.fd = s
    }

    public func close() {
        if fd >= 0 {
            Darwin.close(fd)
            fd = -1
        }
    }

    public var isConnected: Bool { fd >= 0 }

    // MARK: - I/O

    public func send(_ data: Data) throws {
        guard fd >= 0 else { throw IPCError.writeFailed("socket closed") }
        var sent = 0
        let total = data.count
        try data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) -> Void in
            let base = raw.baseAddress!
            while sent < total {
                let n = Darwin.write(fd, base.advanced(by: sent), total - sent)
                if n < 0 {
                    if errno == EINTR { continue }
                    throw IPCError.writeFailed(String(cString: strerror(errno)))
                }
                if n == 0 {
                    throw IPCError.writeFailed("write returned 0")
                }
                sent += n
            }
        }
    }

    /// 读取一个完整帧 (header + payload). 阻塞.
    public func readFrame() throws -> Frame {
        let header = try readExact(WireProtocol.headerSize)
        let (cmd, length, isAsync) = try BinaryCodec.decodeHeader(header)
        let payload: Data
        if length > 0 {
            payload = try readExact(Int(length))
        } else {
            payload = Data()
        }
        return Frame(cmd: cmd, isAsync: isAsync, payload: payload)
    }

    /// 阻塞读 n 字节, 不足 → IPCError.eof.
    private func readExact(_ n: Int) throws -> Data {
        guard fd >= 0 else { throw IPCError.readFailed("socket closed") }
        var buf = Data(count: n)
        var got = 0
        try buf.withUnsafeMutableBytes { (raw: UnsafeMutableRawBufferPointer) -> Void in
            let base = raw.baseAddress!
            while got < n {
                let r = Darwin.read(fd, base.advanced(by: got), n - got)
                if r < 0 {
                    if errno == EINTR { continue }
                    throw IPCError.readFailed(String(cString: strerror(errno)))
                }
                if r == 0 {
                    throw IPCError.eof
                }
                got += r
            }
        }
        return buf
    }
}
