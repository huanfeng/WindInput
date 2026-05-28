import Foundation

// 跨语言协议同步 (必读):
//   Go SSOT     : wind_input/internal/ipc/binary_protocol.go
//   Win 端镜像  : wind_tsf/include/BinaryProtocol.h
//
// 修改任何 cmd id / 帧布局时, 同步三处.

public enum WireProtocol {
    public static let version: UInt16 = 0x1001
    public static let asyncFlag: UInt16 = 0x8000
    public static let headerSize = 8
    public static let maxPayloadSize: UInt32 = 1024 * 1024
}

// MARK: - 上行 cmd (客户端 → Go)

public enum UpstreamCmd {
    public static let keyEvent: UInt16        = 0x0101
    public static let commitRequest: UInt16   = 0x0104
    public static let focusGained: UInt16     = 0x0201
    public static let focusLost: UInt16       = 0x0202
    public static let imeActivated: UInt16    = 0x0203
    public static let imeDeactivated: UInt16  = 0x0204
    public static let modeNotify: UInt16      = 0x0205
    public static let toggleMode: UInt16      = 0x0207
    public static let showContextMenu: UInt16 = 0x020A
    public static let systemModeSwitch: UInt16 = 0x020B
    public static let candidateSelect: UInt16  = 0x020D   // NSPanel 鼠标点击命中候选 (payload: pageLocalIndex u32)
    public static let candidateHover: UInt16   = 0x020E   // NSPanel 鼠标悬停候选 (payload: pageLocalIndex i32, -1=无)
    public static let caretUpdate: UInt16     = 0x0301
    public static let selectionChanged: UInt16 = 0x0302
    public static let caretPending: UInt16    = 0x0303
    public static let batchEvents: UInt16     = 0x0F01
}

// MARK: - 下行 cmd (Go → 客户端)

public enum DownstreamCmd {
    public static let ack: UInt16              = 0x0001
    public static let passThrough: UInt16      = 0x0002
    public static let commitText: UInt16       = 0x0101
    public static let updateComposition: UInt16 = 0x0102
    public static let clearComposition: UInt16 = 0x0103
    public static let commitResult: UInt16     = 0x0105
    public static let commitTextWithCursor: UInt16 = 0x0106
    public static let moveCursor: UInt16       = 0x0107
    public static let deletePair: UInt16       = 0x0108
    public static let consumed: UInt16         = 0x0401
    public static let statusUpdate: UInt16     = 0x0202
    public static let statePush: UInt16        = 0x0206
    public static let serviceReady: UInt16     = 0x0207
    public static let syncHotkeys: UInt16      = 0x0301
    public static let syncConfig: UInt16       = 0x0303
    public static let hostRenderSetup: UInt16  = 0x0501
    public static let hostRenderFrame: UInt16  = 0x0502   // SHM 新帧就绪通知 (darwin)
    public static let candidateRects: UInt16   = 0x0503   // 当前帧候选命中矩形 (panel-local)
    public static let modeStatus: UInt16       = 0x0504   // 输入模式状态 (中英/全半角/标点/方案), 供菜单栏指示器
    public static let batchResponse: UInt16    = 0x0F02
}

/// 输入模式状态 (CmdModeStatus 0x0504 解码结果)。供菜单栏指示器显示。
public struct ModeStatusPayload {
    public let chineseMode: Bool
    public let fullWidth: Bool
    public let chinesePunct: Bool
    public let capsLock: Bool
    public let visible: Bool        // false = 隐藏指示器 (IME 失活/失焦)
    public let effectiveMode: UInt32 // 0=中文 1=英文小写 2=英文大写
    public let modeLabel: String    // 方案标签 ("拼"/"五"/"双"/"混")

    public init(chineseMode: Bool, fullWidth: Bool, chinesePunct: Bool, capsLock: Bool,
                visible: Bool, effectiveMode: UInt32, modeLabel: String) {
        self.chineseMode = chineseMode
        self.fullWidth = fullWidth
        self.chinesePunct = chinesePunct
        self.capsLock = capsLock
        self.visible = visible
        self.effectiveMode = effectiveMode
        self.modeLabel = modeLabel
    }
}

// CandidateHitRect — 单个候选在候选框 bitmap 内的命中矩形 (panel-local 像素).
// 与 Go ipc.CandidateHitRect 镜像。
public struct CandidateHitRect: Equatable {
    public let index: Int32
    public let x: Int32
    public let y: Int32
    public let w: Int32
    public let h: Int32
    public init(index: Int32, x: Int32, y: Int32, w: Int32, h: Int32) {
        self.index = index; self.x = x; self.y = y; self.w = w; self.h = h
    }
    public func contains(px: CGFloat, py: CGFloat) -> Bool {
        return px >= CGFloat(x) && px < CGFloat(x + w) &&
            py >= CGFloat(y) && py < CGFloat(y + h)
    }
}

// HostRenderFramePayload — CmdHostRenderFrame (0x0502) 24 字节 payload.
// 与 Go internal/ipc/binary_protocol.go HostRenderFramePayload 镜像。
public struct HostRenderFramePayload: Equatable {
    public let seq: UInt32
    public let x: Int32           // logical 点 (top-left)
    public let y: Int32
    public let width: UInt32      // device 像素 (= logical × scale)
    public let height: UInt32
    public let flags: UInt32
    public let scale: UInt32      // HiDPI 渲染倍率; logical 尺寸 = 像素/scale (1=非 Retina, 2=Retina)

    public init(seq: UInt32, x: Int32, y: Int32, width: UInt32, height: UInt32,
                flags: UInt32, scale: UInt32 = 1) {
        self.seq = seq; self.x = x; self.y = y
        self.width = width; self.height = height; self.flags = flags
        self.scale = max(1, scale)
    }
}

// MARK: - KeyEvent

public enum KeyEventType: UInt8 {
    case down = 0
    case up   = 1
}

public struct KeyEventPayload: Equatable {
    public var keyCode: UInt32
    public var scanCode: UInt32
    public var modifiers: UInt32
    public var eventType: KeyEventType
    public var toggles: UInt8
    public var eventSeq: UInt16
    public var prevChar: UInt16  // 0 = unavailable

    public init(keyCode: UInt32,
                scanCode: UInt32 = 0,
                modifiers: UInt32 = 0,
                eventType: KeyEventType = .down,
                toggles: UInt8 = 0,
                eventSeq: UInt16 = 0,
                prevChar: UInt16 = 0) {
        self.keyCode = keyCode
        self.scanCode = scanCode
        self.modifiers = modifiers
        self.eventType = eventType
        self.toggles = toggles
        self.eventSeq = eventSeq
        self.prevChar = prevChar
    }
}

// MARK: - 解码后的帧

public struct Frame: Equatable {
    public let cmd: UInt16
    public let isAsync: Bool
    public let payload: Data

    public init(cmd: UInt16, isAsync: Bool, payload: Data) {
        self.cmd = cmd
        self.isAsync = isAsync
        self.payload = payload
    }
}

// MARK: - 错误

public enum IPCError: Error, Equatable {
    case eof
    case versionMismatch(UInt16)
    case payloadTooLarge(UInt32)
    case payloadTooShort(expected: Int, got: Int)
    case connectFailed(String)
    case writeFailed(String)
    case readFailed(String)
}

// MARK: - 默认运行时路径

public enum BridgeEndpoints {
    public static var runtimeDir: String {
        if let env = ProcessInfo.processInfo.environment["WIND_INPUT_RUNTIME_DIR"], !env.isEmpty {
            return env
        }
        return "\(NSHomeDirectory())/Library/Application Support/WindInput"
    }

    public static var requestSocket: String { "\(runtimeDir)/bridge.sock" }
    public static var pushSocket: String    { "\(runtimeDir)/bridge_push.sock" }
}
