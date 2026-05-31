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
    public static let candidateContextMenu: UInt16 = 0x020F // NSPanel 右键菜单动作 (payload: index i32 + actionLen u32 + action UTF-8)
    public static let menuAction: UInt16       = 0x0210   // 统一菜单项被选中 (payload: id i32)
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
    public static let candidateMenuFlags: UInt16 = 0x0505 // 当前页候选右键菜单禁用位 (每候选 1 字节)
    public static let menuShow: UInt16         = 0x0506   // 统一菜单树 (CmdShowContextMenu 请求的响应)
    public static let openSettings: UInt16     = 0x0507   // 请求打开设置应用 (payload: page UTF-8)
    public static let tooltipShow: UInt16      = 0x0508   // 候选悬停 tooltip 文本 + 主题色; .app 据悬停候选矩形定位
    public static let tooltipHide: UInt16      = 0x0509   // 隐藏 tooltip (空 payload)
    public static let statusShow: UInt16       = 0x050A   // 状态提示气泡 (模式/标点/全半角文本 + 主题色 + 位置 + 时长)
    public static let statusHide: UInt16       = 0x050B   // 隐藏状态提示气泡 (空 payload)
    public static let toastShow: UInt16        = 0x050C   // Toast 通知 (标题+正文 + 主题色 + accent + 位置 + 时长)
    public static let toastHide: UInt16        = 0x050D   // 隐藏 Toast (空 payload)
    public static let batchResponse: UInt16    = 0x0F02
}

/// 统一菜单项 (CmdMenuShow 0x0506 解码结果, 树形)。供构建原生 NSMenu。
public struct MenuItemData {
    public let id: Int32
    public let label: String
    public let separator: Bool
    public let checked: Bool
    public let disabled: Bool
    public let children: [MenuItemData]

    public init(id: Int32, label: String, separator: Bool, checked: Bool,
                disabled: Bool, children: [MenuItemData]) {
        self.id = id
        self.label = label
        self.separator = separator
        self.checked = checked
        self.disabled = disabled
        self.children = children
    }
}

/// 候选悬停 tooltip (CmdTooltipShow 0x0508 解码结果)。text 可含 \n 多行、\t 分列。
/// bgColor/fgColor 为 #RRGGBBAA, 空串表示用 .app 内置深色默认。位置由 .app 定。
public struct TooltipPayload {
    public let text: String
    public let bgColor: String
    public let fgColor: String
    public let fontPath: String   // 拆字字根字体文件绝对路径, 空=无需特殊字体

    public init(text: String, bgColor: String, fgColor: String, fontPath: String = "") {
        self.text = text
        self.bgColor = bgColor
        self.fgColor = fgColor
        self.fontPath = fontPath
    }
}

/// 状态提示气泡 (CmdStatusShow 0x050A 解码结果)。模式切换时近 caret 弹出的瞬态气泡。
/// text 为合并短文 (如 "中 ，"); bgColor/fgColor 为 #RRGGBBAA; x/y 为 caret 屏幕坐标
/// (wire top-left); durationMs>0 时到点自动隐藏 (temp), ==0 常驻 (always)。
public struct StatusBubblePayload {
    public let text: String
    public let bgColor: String
    public let fgColor: String
    public let x: Int32
    public let y: Int32
    public let durationMs: Int32

    public init(text: String, bgColor: String, fgColor: String, x: Int32, y: Int32, durationMs: Int32) {
        self.text = text
        self.bgColor = bgColor
        self.fgColor = fgColor
        self.x = x
        self.y = y
        self.durationMs = durationMs
    }
}

/// Toast 通知 (CmdToastShow 0x050C 解码结果)。屏幕级通知 (如词库加载完成), 区别于
/// 锚 caret 的瞬态状态气泡。message 可含 \n 多行; bgColor/fgColor/accentColor 为
/// #RRGGBB[AA]; position 为 "bottom_right"/"center" (.app 据此在工作区落位);
/// durationMs: 0=默认 5000, >0 自动隐藏毫秒数, <0 不自动隐藏; maxWidth 内容最大像素宽
/// (DIP, 逻辑点), 0=由 .app 决定。
public struct ToastPayload {
    public let title: String
    public let message: String
    public let bgColor: String
    public let fgColor: String
    public let accentColor: String
    public let position: String
    public let durationMs: Int32
    public let maxWidth: Int32

    public init(title: String, message: String, bgColor: String, fgColor: String,
                accentColor: String, position: String, durationMs: Int32, maxWidth: Int32) {
        self.title = title
        self.message = message
        self.bgColor = bgColor
        self.fgColor = fgColor
        self.accentColor = accentColor
        self.position = position
        self.durationMs = durationMs
        self.maxWidth = maxWidth
    }
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
    /// 变体后缀: debug 变体的 .app (bundleID 以 "Debug" 结尾) 用 "_debug", 与 Go
    /// buildvariant.Suffix() 对齐, 让 debug/release 两套 .app + 服务各用独立运行时目录
    /// (socket/config) 共存 —— 可同时注册为两个输入法, 日常用正式版、旁边测开发版。
    public static var variantSuffix: String {
        (Bundle.main.bundleIdentifier?.hasSuffix("Debug") ?? false) ? "_debug" : ""
    }

    public static var runtimeDir: String {
        if let env = ProcessInfo.processInfo.environment["WIND_INPUT_RUNTIME_DIR"], !env.isEmpty {
            return env
        }
        return "\(NSHomeDirectory())/Library/Application Support/WindInput\(variantSuffix)"
    }

    public static var requestSocket: String { "\(runtimeDir)/bridge.sock" }
    public static var pushSocket: String    { "\(runtimeDir)/bridge_push.sock" }
}
