import Foundation

// PushClient — 异步订阅 Go 服务的 bridge_push.sock, 后台 read 帧并 callback 给上层.
//
// Bridge push 通道上推的都是标准 bridge IPC 帧 (header + payload), 包含:
//   - 0x0206 CmdStatePush         status push (flags + iconLabel)
//   - 0x0207 CmdServiceReady      服务就绪通知 (空 payload)
//   - 0x0101 CmdCommitText        commit 文本
//   - 0x0102 CmdUpdateComposition 更新 preedit
//   - 0x0103 CmdClearComposition  清 preedit
//   - 0x0F01 CmdBatchEvents       批量 uicmd 帧容器 (内部 N×(header+uicmd payload))
//   - 0x0301 CmdSyncHotkeys / 0x0303 CmdSyncConfig 等
//
// 设计:
//   - 内部持有一个 BridgeClient 连 bridge_push.sock
//   - 后台 DispatchQueue 跑 read loop, 每次 readFrame() 后回调 onFrame(Frame)
//   - InputController 根据 frame.cmd 路由处理 (M2.2-C / D 实装具体动作)
//   - stop() 优雅停止: close socket, read loop 收到 EOF 退出
public final class PushClient {

    public typealias FrameHandler = (Frame) -> Void
    public typealias ErrorHandler = (IPCError) -> Void

    private let socketPath: String
    private let queue: DispatchQueue
    private var client: BridgeClient?

    // 用 lock 保护 state 转换, 避免 stop 与 read loop 竞争
    private let stateLock = NSLock()
    private var _stopRequested = false

    public private(set) var isRunning = false

    /// 收到完整 bridge 帧时回调. 回调发生在内部后台队列, 不要在里面做长操作或
    /// 触发 UI; 如需 UI 操作请 dispatch 到 main.
    public var onFrame: FrameHandler?

    /// 读循环异常退出时回调 (例如 server 断连). 默认 nil. 同样在后台队列.
    public var onError: ErrorHandler?

    public init(socketPath: String = BridgeEndpoints.pushSocket,
                queue: DispatchQueue? = nil) {
        self.socketPath = socketPath
        self.queue = queue ?? DispatchQueue(label: "WindInputKit.PushClient",
                                            qos: .userInitiated)
    }

    deinit {
        stop()
    }

    // MARK: - 启动/停止

    /// 连接 push socket 并开始后台 read loop. 同步连接 (要么连上要么抛错).
    public func start() throws {
        stateLock.lock()
        defer { stateLock.unlock() }
        guard !isRunning else { return }

        let c = try BridgeClient(socketPath: socketPath)
        client = c
        _stopRequested = false
        isRunning = true

        queue.async { [weak self] in
            self?.readLoop()
        }
    }

    /// 停止 read loop. 优雅: 设 stop 标志 + close socket, read loop 下一次 read
    /// 会拿到 EOF 退出. 幂等, 重入安全.
    public func stop() {
        stateLock.lock()
        let already = !isRunning
        _stopRequested = true
        let c = client
        client = nil
        isRunning = false
        stateLock.unlock()

        if already { return }
        c?.close()
    }

    private var stopRequested: Bool {
        stateLock.lock()
        defer { stateLock.unlock() }
        return _stopRequested
    }

    // MARK: - read loop

    private func readLoop() {
        while !stopRequested {
            // 在锁外拿 client 避免持锁阻塞 read
            stateLock.lock()
            let c = client
            stateLock.unlock()
            guard let c = c, c.isConnected else { return }

            do {
                let frame = try c.readFrame()
                onFrame?(frame)
            } catch IPCError.eof {
                // server 关 socket 或 stop() 主动 close, 正常退出
                return
            } catch let e as IPCError {
                if !stopRequested {
                    onError?(e)
                }
                return
            } catch {
                if !stopRequested {
                    onError?(.readFailed(String(describing: error)))
                }
                return
            }
        }
    }
}
