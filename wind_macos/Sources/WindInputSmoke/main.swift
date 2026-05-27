import Foundation
import WindInputKit

// wind-smoke — PR-A M1 协议通路验收工具.
//
// 行为:
//   1. 连 bridge.sock, 发一帧 KeyEvent (KeyCode = 0x41 'A', down), 收响应打印
//   2. 连 bridge_push.sock, 阻塞读 N 秒, 把所有 push 帧 cmd id/len 打印
//
// 用法:
//   swift run wind-smoke [seconds]   # 默认 10 秒
//
// 这一步只验证 "Swift codec + UDS client 能与 Go 服务对话",
// 不涉及 IMKit 输入流程.

let pushReadSeconds: TimeInterval = {
    if CommandLine.arguments.count >= 2, let v = TimeInterval(CommandLine.arguments[1]) {
        return v
    }
    return 10
}()

let requestPath = BridgeEndpoints.requestSocket
let pushPath    = BridgeEndpoints.pushSocket

print("[smoke] runtime dir : \(BridgeEndpoints.runtimeDir)")
print("[smoke] request sock: \(requestPath)")
print("[smoke] push sock   : \(pushPath)")

// MARK: 1. KeyEvent roundtrip

do {
    print("[smoke] === KeyEvent roundtrip ===")
    let client = try BridgeClient(socketPath: requestPath)
    let frame = BinaryCodec.encodeKeyEventFrame(KeyEventPayload(
        keyCode: 0x41,          // VK 'A'
        modifiers: 0,
        eventType: .down,
        eventSeq: 1
    ))
    print("[smoke] -> send  KeyEvent  bytes=\(frame.count) hex=\(frame.hexString())")
    try client.send(frame)

    let resp = try client.readFrame()
    print(String(format: "[smoke] <- recv  cmd=0x%04x len=%d isAsync=%@ payloadHex=%@",
                 resp.cmd, resp.payload.count,
                 resp.isAsync ? "true" : "false",
                 resp.payload.hexString()))
    client.close()
} catch {
    print("[smoke] !! request channel error: \(error)")
}

// MARK: 2. Push channel subscribe

print("[smoke] === Push channel (\(Int(pushReadSeconds))s) ===")
do {
    let pushClient = try BridgeClient(socketPath: pushPath)

    let deadline = Date().addingTimeInterval(pushReadSeconds)
    let queue = DispatchQueue(label: "wind-smoke.push")
    let group = DispatchGroup()
    group.enter()

    queue.async {
        while Date() < deadline {
            do {
                let f = try pushClient.readFrame()
                print(String(format: "[smoke] push cmd=0x%04x len=%d payload=%@",
                             f.cmd, f.payload.count, f.payload.prefix(48).hexString()))
            } catch IPCError.eof {
                print("[smoke] push EOF (server closed)")
                break
            } catch {
                print("[smoke] !! push error: \(error)")
                break
            }
        }
        group.leave()
    }

    // 超时兜底: 主线程 sleep 到 deadline 后关 socket, push goroutine 在下一次 read 时报 EOF/EBADF.
    Thread.sleep(until: deadline)
    pushClient.close()
    _ = group.wait(timeout: .now() + 1.0)
} catch {
    print("[smoke] !! push connect error: \(error)")
}

print("[smoke] done")

// MARK: - Helpers

extension Data {
    func hexString() -> String {
        return map { String(format: "%02x", $0) }.joined()
    }
}
