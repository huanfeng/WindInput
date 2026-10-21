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
    /// NSPanel 候选框滚轮 (payload: delta i32, WHEEL_DELTA=120 的倍数, 正=上滚)。
    /// 服务端把它解释成「上下键调整高亮项」(到页边界翻到相邻页)，两平台同一实现。
    public static let candidateScroll: UInt16  = 0x0211
    /// 自绘菜单打开期间的原始指针事件 (payload 16 字节: event u32 + button u32 + x i32 + y i32,
    /// wire 点坐标)。event: 1 移动 / 2 按下；button: 1 左 / 2 中 / 3 右，移动时 0。
    public static let menuPointer: UInt16      = 0x021B
    public static let menuAction: UInt16       = 0x0210   // 统一菜单项被选中 (payload: id i32)
    public static let frontContext: UInt16     = 0x0215   // 前台上下文快照 (payload: appLen+app + titleLen+title + selLen+sel)
    /// 扩展信封 (上行)。低频消息的统一入口，见 Rust protocol.rs 的 CMD_EXT。
    /// 布局: kindLen u32 + kind(UTF-8) + bodyLen u32 + body(任意字节, 通常 JSON)。
    public static let ext: UInt16              = 0x0E01
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
    /// 收组合 + 把**当前这个键**交还宿主 (联想态回车/退格透传)。无载荷。
    /// Win 侧要靠 SendInput 重放才能不丢键; IMKit 无前置闸门, 清完 marked text
    /// `return false` 即可 —— 同一个意图, 两种手段。
    public static let clearThenPassThrough: UInt16 = 0x010D
    public static let commitResult: UInt16     = 0x0105
    public static let commitTextWithCursor: UInt16 = 0x0106
    public static let moveCursor: UInt16       = 0x0107
    public static let deletePair: UInt16       = 0x0108
    public static let replaceBackward: UInt16  = 0x0109   // 删除光标前 N 字符 + 插入文本 (智能符号)
    // ── 延迟组合三兄弟 (timeout_ms 前缀) ──
    // 名字带 "hold/defer" 是 TSF 的说法: Win 侧 C++ 必须把组合「吃了再吐」, 故要计时器兜底。
    // IMKit 无此约束 (insertText 后可立即 setMarkedText), 故 .app 侧只用文本、忽略 timeout。
    // ⚠️ 这三条**不是** Windows 专有: commitThenDefer 由码表顶码 direct_commit 产生
    // (handle_candidate.rs), 是跨平台通路 —— 早期 macOS 侧漏接, 顶码上屏的字被 router
    // 的 default 分支静默吞掉 (返回 true 消费按键却不出字)。
    public static let holdComposition: UInt16  = 0x010A   // timeout_ms u32 + textLen u32 + text
    public static let commitAndHold: UInt16    = 0x010B   // timeout_ms u32 + commitLen u32 + holdLen u32 + 两段文本
    public static let commitThenDefer: UInt16  = 0x010C   // timeout_ms u32 + commitLen u32 + deferLen u32 + 两段文本
    public static let consumed: UInt16         = 0x0401
    public static let statusUpdate: UInt16     = 0x0202
    public static let statePush: UInt16        = 0x0206
    public static let serviceReady: UInt16     = 0x0207
    public static let syncHotkeys: UInt16      = 0x0301
    public static let syncConfig: UInt16       = 0x0303
    public static let hostRenderSetup: UInt16  = 0x0501
    public static let hostRenderFrame: UInt16  = 0x0502   // SHM 新帧就绪通知 (darwin)
    public static let candidateRects: UInt16   = 0x0503   // 当前帧候选命中矩形 (panel-local)
    /// 光栅浮层帧 (状态气泡 / Toast / 悬停提示 / 自绘菜单)：像素在各层自己的 SHM 段，本帧带落位规则。
    /// 布局见 `OverlayFrame`。
    public static let overlayFrame: UInt16     = 0x0513
    public static let modeStatus: UInt16       = 0x0504   // 输入模式状态 (中英/全半角/标点/方案), 供菜单栏指示器
    public static let menuShow: UInt16         = 0x0506   // 统一菜单树 (CmdShowContextMenu 请求的响应)
    /// 扩展信封 (下行)。与上行同码位、按方向区分，见 Rust protocol.rs 的 CMD_EXT。
    /// 「打开设置」等低频消息走这里 (kind = "settings.open")，不再各占一个码位。
    public static let ext: UInt16              = 0x0E01
    public static let keyTap: UInt16           = 0x050E   // 命令直通车单次按键合成 (key + modifiers); CGEvent post
    public static let keySeq: UInt16           = 0x050F   // 顺序多个按键组合
    public static let keyHold: UInt16          = 0x0510   // 按下并保持 (与 release 成对)
    public static let keyRelease: UInt16       = 0x0511   // 抬起之前 hold 的组合
    public static let keyType: UInt16          = 0x0512   // Unicode 文本上屏 (走 client.insertText, 非 CGEvent)
    public static let batchResponse: UInt16    = 0x0F02
}

/// 扩展信封的 kind 常量。**须与 Rust `wind_ipc::protocol::ext_kind` 逐字一致**——
/// 两端拼写不一致的错误只会表现为「消息静默丢失」，没有任何报错。
public enum ExtKind {
    /// 下行：请求打开设置应用。body = `{"args":["--page=dict", …]}`。
    public static let settingsOpen = "settings.open"
    /// 上行：候选窗被拖动到新位置。body = `{"x":123,"y":456}`，wire 坐标系（屏幕左上为
    /// 原点、y 向下）下的**内容左上角**，与配置里的 `ui.candidate.custom_x/y` 同义。
    public static let posCandidate = "pos.candidate"
    /// 上行：状态提示气泡被拖动到新位置。body 同 `posCandidate`。
    public static let posStatusTip = "pos.status_tip"
    /// 下行：问 `.app` 候选窗此刻在哪，答案走上行 `posCandidate`。body 空。
    public static let posCandidateQuery = "pos.candidate.query"
    /// 下行：问 `.app` 状态气泡此刻在哪，答案走上行 `posStatusTip`。body 空。
    public static let posStatusTipQuery = "pos.status_tip.query"
    //
    // 这两个「位置」要一问一答而不是由服务进程记账：浮窗落位在 `.app` 侧，服务端发下来的只是
    // **建议落点**，这边还会按所在屏可见区钳制、在下方放不下时翻到光标上方、以及沿用用户本次
    // 组合内拖出来的落位。用户点「固定位置」要以当前看到的位置落盘。
    /// 上行：请求打开自绘菜单。body = `{"target":<页内下标>|-1 主菜单|-2 状态气泡|-3 悬停提示,
    /// "x":..,"y":..,"work":[左,上,右,下]}`，坐标为 wire 点；悬停提示另带 `"lx","ly"`（右键点在提示
    /// 位图内的像素坐标，含软阴影扩边）。
    public static let menuOpen = "menu.open"
    /// 上行：`.app` 自己把菜单收掉了（空闲超时 / 服务重启 / 断线…），服务端据此复位。
    /// body = `{"reason":"…"}`，只进日志。
    public static let menuDismiss = "menu.dismiss"
}

/// 按键组合 (CmdKeyTap/Hold/Release 解码结果, 及 KeySeq 内单项)。
/// key 为规范键名 (如 "a"/"enter"/"left"/"home"/"vk:0x5D"); modifiers 为
/// {"ctrl","shift","alt","win"} 子集 (win 在 .app 侧映射为 Command)。
/// 与 Go internal/uicmd.KeyCombo / KeyTapPayload 镜像。
public struct KeyComboPayload {
    public let key: String
    public let modifiers: [String]

    public init(key: String, modifiers: [String]) {
        self.key = key
        self.modifiers = modifiers
    }
}

/// 顺序多个按键组合 (CmdKeySeq 0x050F 解码结果)。
public struct KeySeqPayload {
    public let combos: [KeyComboPayload]

    public init(combos: [KeyComboPayload]) {
        self.combos = combos
    }
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

    /// 帧可见 (SharedRenderHeader::FLAG_VISIBLE)。
    public var isVisible: Bool { flags & 0x1 != 0 }
    /// 位图内已画软件高斯阴影 → 关掉系统窗口阴影, 否则画布边缘出黑边。
    public var hasSoftwareShadow: Bool { flags & 0x4 != 0 }
    /// `(x, y)` 是用户**固定位置**的绝对屏幕坐标, 不是按光标推算的落点
    /// (SharedRenderHeader::FLAG_ABSOLUTE_POS)。
    ///
    /// 置位时 panel 只做屏幕边界钳制, 不套用「下方放不下就翻到光标上方」的兜底——窗口
    /// 本来就不跟光标走, 固定点一旦靠近屏幕底边就会被那套逻辑莫名弹到顶上。
    public var isAbsolutePos: Bool { flags & 0x8 != 0 }
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
    /// 收发超时 (服务卡住/不回包; 此时连接已被关闭), 或处于超时后的冷却期。重试无益。
    case timeout
}

// MARK: - 默认运行时路径

public enum BridgeEndpoints {
    /// request 连接 I/O 超时 (毫秒): 服务卡死/与 app 版本不配套时, 主线程上的同步 readFrame
    /// 否则会无限阻塞 → 切到本输入法整机卡死。正常 UDS roundtrip <1ms; 超时后调用方丢弃连接重建。
    /// 凡在主线程做同步 roundtrip 的连接都要带上; push 连接不设 (见 BridgeClient.ioTimeoutMs)。
    public static let requestIOTimeoutMs = 2000
    /// 超时后的冷却期 (秒): 期内带超时的连接直接抛 .timeout, 不再逐个等满超时。
    public static let requestStallCooldown: TimeInterval = 5

    /// 变体后缀: dev 变体的 .app (bundleID 以 "Dev" 结尾) 用 "Dev", 与 Rust
    /// wind_config::variant::app_dir_name() 对齐 (release=WindInput, dev=WindInputDev),
    /// 让 dev/release 两套 .app + 服务各用独立运行时目录 (socket/config) 共存 ——
    /// 可同时注册为两个输入法, 日常用正式版、旁边测开发版。
    public static var variantSuffix: String {
        (Bundle.main.bundleIdentifier?.hasSuffix("Dev") ?? false) ? "Dev" : ""
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
