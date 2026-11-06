import Foundation

// BinaryCodec — wind_input/internal/ipc/binary_codec.go 的 Swift 镜像.
//
// 字节布局 (all little-endian):
//   Header (8 bytes): u16 version | u16 cmd | u32 length
//   KeyEvent payload (18 bytes): u32 keyCode | u32 scanCode | u32 modifiers
//                                | u8 type | u8 toggles | u16 seq | u16 prevChar
//
// version 字段:
//   - 高 4 位是 major version, 必须等于 ProtocolVersion >> 12 (= 0x1)
//   - 高 1 位 (0x8000) 是 AsyncFlag, 上行帧标记 "无需响应"
//   - 校验时先剥 AsyncFlag, 再比 major
public enum BinaryCodec {

    // MARK: - Encode Header

    public static func encodeHeader(cmd: UInt16, payloadLen: UInt32, async: Bool = false) -> Data {
        var buf = Data(count: WireProtocol.headerSize)
        var ver = WireProtocol.version
        if async {
            ver |= WireProtocol.asyncFlag
        }
        buf.writeUInt16LE(ver, at: 0)
        buf.writeUInt16LE(cmd, at: 2)
        buf.writeUInt32LE(payloadLen, at: 4)
        return buf
    }

    // MARK: - Decode Header

    public static func decodeHeader(_ buf: Data) throws -> (cmd: UInt16, length: UInt32, isAsync: Bool) {
        guard buf.count >= WireProtocol.headerSize else {
            throw IPCError.payloadTooShort(expected: WireProtocol.headerSize, got: buf.count)
        }
        let ver = buf.readUInt16LE(at: 0)
        let cmd = buf.readUInt16LE(at: 2)
        let length = buf.readUInt32LE(at: 4)
        let isAsync = (ver & WireProtocol.asyncFlag) != 0
        let base = ver & ~WireProtocol.asyncFlag
        guard (base >> 12) == (WireProtocol.version >> 12) else {
            throw IPCError.versionMismatch(ver)
        }
        guard length <= WireProtocol.maxPayloadSize else {
            throw IPCError.payloadTooLarge(length)
        }
        return (cmd, length, isAsync)
    }

    // MARK: - CaretUpdate payload (upstream)

    /// 编码 CmdCaretUpdate (0x0301 upstream) 帧.
    /// 布局: header(8) + payload {
    ///   x:i32 (4) + y:i32 (4) + height:i32 (4)
    ///   [+ compositionStartX:i32 (4) + compositionStartY:i32 (4)]   // 可选 20 字节版
    /// }
    /// 坐标系: top-left 原点 (与 Go/Win 端一致, 与 Cocoa NSRect 的 bottom-left 不同,
    /// 调用方必须先转换好再传入).
    public static func encodeCaretUpdateFrame(x: Int32, y: Int32, height: Int32,
                                              compositionStartX: Int32? = nil,
                                              compositionStartY: Int32? = nil) -> Data {
        let withExt = (compositionStartX != nil && compositionStartY != nil)
        let payloadLen = withExt ? 20 : 12
        var payload = Data(count: payloadLen)
        payload.writeUInt32LE(UInt32(bitPattern: x), at: 0)
        payload.writeUInt32LE(UInt32(bitPattern: y), at: 4)
        payload.writeUInt32LE(UInt32(bitPattern: height), at: 8)
        if withExt {
            payload.writeUInt32LE(UInt32(bitPattern: compositionStartX!), at: 12)
            payload.writeUInt32LE(UInt32(bitPattern: compositionStartY!), at: 16)
        }

        var out = encodeHeader(cmd: UpstreamCmd.caretUpdate, payloadLen: UInt32(payloadLen))
        out.append(payload)
        return out
    }

    // MARK: - KeyEvent payload

    public static func encodeKeyEventFrame(_ p: KeyEventPayload) -> Data {
        var payload = Data(count: 18)
        payload.writeUInt32LE(p.keyCode,   at: 0)
        payload.writeUInt32LE(p.scanCode,  at: 4)
        payload.writeUInt32LE(p.modifiers, at: 8)
        payload[12] = p.eventType.rawValue
        payload[13] = p.toggles
        payload.writeUInt16LE(p.eventSeq, at: 14)
        payload.writeUInt16LE(p.prevChar, at: 16)

        var out = encodeHeader(cmd: UpstreamCmd.keyEvent, payloadLen: UInt32(payload.count))
        out.append(payload)
        return out
    }

    public static func decodeKeyEventPayload(_ buf: Data) throws -> KeyEventPayload {
        guard buf.count >= 16 else {
            throw IPCError.payloadTooShort(expected: 16, got: buf.count)
        }
        let keyCode   = buf.readUInt32LE(at: 0)
        let scanCode  = buf.readUInt32LE(at: 4)
        let modifiers = buf.readUInt32LE(at: 8)
        let evtRaw    = buf[buf.startIndex + 12]
        let toggles   = buf[buf.startIndex + 13]
        let seq       = buf.readUInt16LE(at: 14)
        let prevChar: UInt16 = buf.count >= 18 ? buf.readUInt16LE(at: 16) : 0

        return KeyEventPayload(
            keyCode: keyCode,
            scanCode: scanCode,
            modifiers: modifiers,
            eventType: KeyEventType(rawValue: evtRaw) ?? .down,
            toggles: toggles,
            eventSeq: seq,
            prevChar: prevChar
        )
    }

    // MARK: - Empty-payload frames (Ack / PassThrough / Consumed / FocusLost / ToggleMode 等)

    public static func encodeEmptyFrame(cmd: UInt16, async: Bool = false) -> Data {
        return encodeHeader(cmd: cmd, payloadLen: 0, async: async)
    }

    /// 编码只带 clientToken(u64 LE) 的帧，用于 CMD_IME_ACTIVATED / CMD_IME_DEACTIVATED。
    ///
    /// 布局与 Windows TSF 端一致：payload 就是 8 字节 token。macOS 以前只发
    /// focusGained / focusLost，服务端的 `handle_ime_activated` 从未被调用，于是
    /// `input.default.remember_last_state = false` 的「每次激活重置中英」不生效。
    public static func encodeTokenFrame(cmd: UInt16, clientToken: UInt64, async: Bool = false) -> Data {
        var payload = Data(count: 8)
        payload.writeUInt32LE(UInt32(truncatingIfNeeded: clientToken), at: 0)
        payload.writeUInt32LE(UInt32(truncatingIfNeeded: clientToken >> 32), at: 4)
        var out = encodeHeader(cmd: cmd, payloadLen: UInt32(payload.count), async: async)
        out.append(payload)
        return out
    }

    /// 编码 CmdShowContextMenu 请求。simplified=true → 追加 1 字节 [1]（服务端据此返回 IMK
    /// 精简菜单，无子菜单）；false → 空 payload（完整菜单，带子菜单，供候选框右键/菜单栏指示器）。
    public static func encodeShowContextMenuFrame(simplified: Bool) -> Data {
        if !simplified {
            return encodeEmptyFrame(cmd: UpstreamCmd.showContextMenu)
        }
        var out = encodeHeader(cmd: UpstreamCmd.showContextMenu, payloadLen: 1)
        out.append(1)
        return out
    }

    /// 编码 CmdFocusGained (0x0201 upstream) 帧。布局与 Windows TSF 端**同构**, 尾部追加
    /// darwin 专属的 bundleID 段:
    /// ```
    ///   caret:20            // x/y/height/compositionStartX/Y (i32 ×5)
    ///   clientToken:u64     // 高 32 位 = 宿主 pid, 低 32 位 = client 实例标识
    ///   inputScopeMask:u64  // TSF InputScope bitmask; macOS 仅用 bit31 (IS_PASSWORD)
    ///   disabled:u8         // TSF compartment 禁用态, macOS 恒 0
    ///   reason:u8           // 同上, 恒 0
    ///   caretSource:u8      // caret_source::UNKNOWN(0) —— macOS 无 TSF 语义域
    ///   bundleIdLen:u32 + bundleId  // darwin 追加段, Windows DLL 不发
    /// ```
    ///
    /// ⚠️ **必须发满 39 字节**。此前只发 12 字节 (pid 占位 + mask), 而 Rust
    /// `FocusGainedPayload::from_bytes` 的下限是 36 —— 解码恒失败, 于是 FOCUS_GAINED 的
    /// **整个重型段从未在 macOS 上执行过**: 按应用初始模式 / compat.toml 规则 / 焦点状态气泡 /
    /// 密码框强制英文 全部静默失效 (那句"darwin 无 PID 概念"的注释是错的, macOS 有 pid)。
    ///
    /// caret 段全 0 是安全的: 服务端 `apply_focus_caret` 在 `height == 0` 时直接返回,
    /// 不会污染坐标缓存 —— macOS 的插入点坐标另有 CmdCaretUpdate 通路。
    ///
    /// - Parameter bundleID: 宿主 app 的 bundle id (取自 IMKit `client.bundleIdentifier()`),
    ///   服务端小写后当作「进程名」用于 compat.toml 匹配与 per-app 记忆。空串 = 未知。
    public static func encodeFocusGainedFrame(
        clientToken: UInt64, inputScopeMask: UInt64, bundleID: String
    ) -> Data {
        let bundleBytes = Array(bundleID.utf8)
        var payload = Data(count: 43)
        // [0, 20) caret 全 0 (height=0 → 服务端忽略)
        payload.writeUInt32LE(UInt32(truncatingIfNeeded: clientToken), at: 20)
        payload.writeUInt32LE(UInt32(truncatingIfNeeded: clientToken >> 32), at: 24)
        payload.writeUInt32LE(UInt32(truncatingIfNeeded: inputScopeMask), at: 28)
        payload.writeUInt32LE(UInt32(truncatingIfNeeded: inputScopeMask >> 32), at: 32)
        payload[36] = 0 // disabled
        payload[37] = 0 // reason
        payload[38] = 0 // caretSource = UNKNOWN
        payload.writeUInt32LE(UInt32(bundleBytes.count), at: 39)
        payload.append(contentsOf: bundleBytes)
        var out = encodeHeader(cmd: UpstreamCmd.focusGained, payloadLen: UInt32(payload.count))
        out.append(payload)
        return out
    }

    // MARK: - Downstream payload decoders (Go → IME)

    // CommitText flags (与 ipc/binary_codec.go: CommitFlagXxx 对齐)
    public static let commitFlagModeChanged: UInt32       = 0x0001
    public static let commitFlagHasNewComposition: UInt32 = 0x0002
    public static let commitFlagChineseMode: UInt32       = 0x0004

    public struct CommitTextPayload: Equatable {
        public let flags: UInt32
        public let text: String              // 要插入的文本
        public let newComposition: String    // 可选: commit 后新的 preedit (内联模式才非空)
        public var modeChanged: Bool       { (flags & BinaryCodec.commitFlagModeChanged)       != 0 }
        public var hasNewComposition: Bool { (flags & BinaryCodec.commitFlagHasNewComposition) != 0 }
        public var chineseMode: Bool       { (flags & BinaryCodec.commitFlagChineseMode)       != 0 }
    }

    /// 解 CmdCommitText payload (0x0101 downstream).
    /// 布局: flags:u32 + textLen:u32 + compLen:u32 + text:bytes + composition:bytes
    public static func decodeCommitTextPayload(_ buf: Data) throws -> CommitTextPayload {
        guard buf.count >= 12 else {
            throw IPCError.payloadTooShort(expected: 12, got: buf.count)
        }
        let flags    = buf.readUInt32LE(at: 0)
        let textLen  = Int(buf.readUInt32LE(at: 4))
        let compLen  = Int(buf.readUInt32LE(at: 8))
        guard buf.count >= 12 + textLen + compLen else {
            throw IPCError.payloadTooShort(expected: 12 + textLen + compLen, got: buf.count)
        }
        let textStart = buf.startIndex + 12
        let compStart = textStart + textLen
        let text = String(data: buf.subdata(in: textStart..<compStart), encoding: .utf8) ?? ""
        let comp = String(data: buf.subdata(in: compStart..<(compStart + compLen)), encoding: .utf8) ?? ""
        return CommitTextPayload(flags: flags, text: text, newComposition: comp)
    }

    /// 已被 `CommitTextReplacingHeld` 消费的 held 组合标记 (Rust encode_commit_text_inner
    /// 的 replacing_held 位)。置位表示这次上屏是在替换先前 hold 住的组合文本,
    /// .app 须先清掉 marked text 再插入, 否则 held 预览与新文本会同时留在宿主里。
    public static let commitFlagReplacingHeld: UInt32 = 0x0008

    /// 延迟组合三兄弟的公共解码结果。`hold`/`defer` 段语义随 cmd 而异:
    /// - holdComposition: commit 为空, hold = 要显示的组合文本
    /// - commitAndHold:   commit 先上屏, hold = 随后开启的组合文本
    /// - commitThenDefer: commit 先上屏, hold = 延迟开启的余码组合
    public struct DeferredCompositionPayload: Equatable {
        public let timeoutMs: UInt32
        public let commitText: String
        public let holdText: String
    }

    /// 解 CmdHoldComposition (0x010A). 布局: timeoutMs:u32 + textLen:u32 + text
    public static func decodeHoldCompositionPayload(_ buf: Data) throws -> DeferredCompositionPayload {
        guard buf.count >= 8 else {
            throw IPCError.payloadTooShort(expected: 8, got: buf.count)
        }
        let timeout = buf.readUInt32LE(at: 0)
        let textLen = Int(buf.readUInt32LE(at: 4))
        guard buf.count >= 8 + textLen else {
            throw IPCError.payloadTooShort(expected: 8 + textLen, got: buf.count)
        }
        let start = buf.startIndex + 8
        let text = String(data: buf.subdata(in: start..<(start + textLen)), encoding: .utf8) ?? ""
        return DeferredCompositionPayload(timeoutMs: timeout, commitText: "", holdText: text)
    }

    /// 解 CmdCommitAndHold (0x010B) / CmdCommitThenDefer (0x010C) —— 两者线格式相同.
    /// 布局: timeoutMs:u32 + commitLen:u32 + holdLen:u32 + commit + hold
    public static func decodeCommitAndHoldPayload(_ buf: Data) throws -> DeferredCompositionPayload {
        guard buf.count >= 12 else {
            throw IPCError.payloadTooShort(expected: 12, got: buf.count)
        }
        let timeout   = buf.readUInt32LE(at: 0)
        let commitLen = Int(buf.readUInt32LE(at: 4))
        let holdLen   = Int(buf.readUInt32LE(at: 8))
        guard buf.count >= 12 + commitLen + holdLen else {
            throw IPCError.payloadTooShort(expected: 12 + commitLen + holdLen, got: buf.count)
        }
        let commitStart = buf.startIndex + 12
        let holdStart   = commitStart + commitLen
        let commit = String(data: buf.subdata(in: commitStart..<holdStart), encoding: .utf8) ?? ""
        let hold   = String(data: buf.subdata(in: holdStart..<(holdStart + holdLen)), encoding: .utf8) ?? ""
        return DeferredCompositionPayload(timeoutMs: timeout, commitText: commit, holdText: hold)
    }

    public struct UpdateCompositionPayload: Equatable {
        public let caretPos: UInt32   // preedit 内光标位置 (UTF-16 unit 还是 rune, 看 Go 端约定; M2.2 阶段照 Go 端原样上送)
        public let text: String       // preedit 文本
    }

    /// 解 CmdUpdateComposition payload (0x0102 downstream).
    /// 布局: caretPos:u32 + text:bytes(剩余)
    public static func decodeUpdateCompositionPayload(_ buf: Data) throws -> UpdateCompositionPayload {
        guard buf.count >= 4 else {
            throw IPCError.payloadTooShort(expected: 4, got: buf.count)
        }
        let caret = buf.readUInt32LE(at: 0)
        let textStart = buf.startIndex + 4
        let text = String(data: buf.subdata(in: textStart..<buf.endIndex), encoding: .utf8) ?? ""
        return UpdateCompositionPayload(caretPos: caret, text: text)
    }

    public struct CommitTextWithCursorPayload: Equatable {
        public let text: String
        public let cursorOffset: UInt32   // 从文本末尾向左偏移的字符数
    }

    /// 解 CmdCommitTextWithCursor payload (0x0106 downstream).
    /// 布局: textLen:u32 + cursorOffset:u32 + text:bytes
    public static func decodeCommitTextWithCursorPayload(_ buf: Data) throws -> CommitTextWithCursorPayload {
        guard buf.count >= 8 else {
            throw IPCError.payloadTooShort(expected: 8, got: buf.count)
        }
        let textLen = Int(buf.readUInt32LE(at: 0))
        let cursor  = buf.readUInt32LE(at: 4)
        guard buf.count >= 8 + textLen else {
            throw IPCError.payloadTooShort(expected: 8 + textLen, got: buf.count)
        }
        let textStart = buf.startIndex + 8
        let text = String(data: buf.subdata(in: textStart..<(textStart + textLen)), encoding: .utf8) ?? ""
        return CommitTextWithCursorPayload(text: text, cursorOffset: cursor)
    }

    public struct MoveCursorPayload: Equatable {
        public let direction: UInt32   // 1 = right, ...
    }

    /// 解 CmdMoveCursor payload (0x0107 downstream).
    /// 布局: direction:u32 (1=right)
    public static func decodeMoveCursorPayload(_ buf: Data) throws -> MoveCursorPayload {
        guard buf.count >= 4 else {
            throw IPCError.payloadTooShort(expected: 4, got: buf.count)
        }
        return MoveCursorPayload(direction: buf.readUInt32LE(at: 0))
    }

    public struct ReplaceBackwardPayload: Equatable {
        public let count: UInt32
        public let text: String
    }

    /// 解 CmdReplaceBackward payload (0x0109 downstream).
    /// 布局: count:u32 + textLength:u32 + text:bytes
    public static func decodeReplaceBackwardPayload(_ buf: Data) throws -> ReplaceBackwardPayload {
        guard buf.count >= 8 else {
            throw IPCError.payloadTooShort(expected: 8, got: buf.count)
        }
        let count = buf.readUInt32LE(at: 0)
        let textLen = Int(buf.readUInt32LE(at: 4))
        guard buf.count >= 8 + textLen else {
            throw IPCError.payloadTooShort(expected: 8 + textLen, got: buf.count)
        }
        let textStart = buf.startIndex + 8
        let text = String(data: buf.subdata(in: textStart..<(textStart + textLen)), encoding: .utf8) ?? ""
        return ReplaceBackwardPayload(count: count, text: text)
    }

    public struct StatePushPayload: Equatable {
        public let flags: UInt32
        public let iconLabel: String

        public var chineseMode: Bool       { (flags & 0x0001) != 0 }   // StatusChineseMode
        public var fullWidth: Bool         { (flags & 0x0002) != 0 }
        public var chinesePunct: Bool      { (flags & 0x0004) != 0 }
        public var toolbarVisible: Bool    { (flags & 0x0008) != 0 }
        public var capsLock: Bool          { (flags & 0x0020) != 0 }
    }

    /// 解 CmdStatePush payload (0x0206 push). 布局:
    /// flags:u32 + keyDownCount:u32 + keyUpCount:u32 + iconLabel:bytes(剩余)
    public static func decodeStatePushPayload(_ buf: Data) throws -> StatePushPayload {
        guard buf.count >= 12 else {
            throw IPCError.payloadTooShort(expected: 12, got: buf.count)
        }
        let flags = buf.readUInt32LE(at: 0)
        let labelStart = buf.startIndex + 12
        let label = String(data: buf.subdata(in: labelStart..<buf.endIndex), encoding: .utf8) ?? ""
        return StatePushPayload(flags: flags, iconLabel: label)
    }

    /// 解 CmdHostRenderFrame payload (0x0502, push). 布局 24 字节 LE:
    /// seq:u32 + x:i32 + y:i32 + w:u32 + h:u32 + flags:u32
    public static func decodeHostRenderFramePayload(_ buf: Data) throws -> HostRenderFramePayload {
        guard buf.count >= 24 else {
            throw IPCError.payloadTooShort(expected: 24, got: buf.count)
        }
        // scale 是 28 字节版的扩展字段; 旧 24 字节帧默认 scale=1。
        let scale = buf.count >= 28 ? buf.readUInt32LE(at: 24) : 1
        return HostRenderFramePayload(
            seq: buf.readUInt32LE(at: 0),
            x: Int32(bitPattern: buf.readUInt32LE(at: 4)),
            y: Int32(bitPattern: buf.readUInt32LE(at: 8)),
            width: buf.readUInt32LE(at: 12),
            height: buf.readUInt32LE(at: 16),
            flags: buf.readUInt32LE(at: 20),
            scale: scale
        )
    }

    /// 编码 CmdCandidateSelect (0x020D upstream): payload = pageLocalIndex u32 LE。
    public static func encodeCandidateSelectFrame(index: Int) -> Data {
        var payload = Data(count: 4)
        payload.writeUInt32LE(UInt32(max(0, index)), at: 0)
        var out = encodeHeader(cmd: UpstreamCmd.candidateSelect, payloadLen: 4)
        out.append(payload)
        return out
    }

    /// 编码 CmdCandidateScroll (0x0211 upstream): payload = delta i32 LE。
    ///
    /// `delta` 用 Win32 的 `WHEEL_DELTA`(120) 倍数、正=上滚 —— 这是既有的 wire 约定
    /// (Windows 的 host-render DLL 原样转发系统滚轮消息)，macOS 侧折算成同一单位，
    /// 服务端因此只需一份实现。
    public static func encodeCandidateScrollFrame(delta: Int32) -> Data {
        var payload = Data(count: 4)
        payload.writeUInt32LE(UInt32(bitPattern: delta), at: 0)
        var out = encodeHeader(cmd: UpstreamCmd.candidateScroll, payloadLen: 4)
        out.append(payload)
        return out
    }

    /// 编码 CmdCandidateHover (0x020E upstream): payload = pageLocalIndex i32 LE (-1=无)。
    public static func encodeCandidateHoverFrame(index: Int) -> Data {
        var payload = Data(count: 4)
        payload.writeUInt32LE(UInt32(bitPattern: Int32(index)), at: 0)
        var out = encodeHeader(cmd: UpstreamCmd.candidateHover, payloadLen: 4)
        out.append(payload)
        return out
    }

    /// 编码 CmdMenuAction (0x0210 upstream): payload = id i32 LE。
    public static func encodeMenuActionFrame(id: Int32) -> Data {
        var payload = Data(count: 4)
        payload.writeUInt32LE(UInt32(bitPattern: id), at: 0)
        var out = encodeHeader(cmd: UpstreamCmd.menuAction, payloadLen: 4)
        out.append(payload)
        return out
    }

    // readLenString 读 u32 长度前缀的 UTF-8 字符串, 推进 off。按键解码共用。
    private static func readLenString(_ buf: Data, _ off: inout Int) throws -> String {
        guard buf.count >= off + 4 else {
            throw IPCError.payloadTooShort(expected: off + 4, got: buf.count)
        }
        let n = Int(buf.readUInt32LE(at: off)); off += 4
        guard buf.count >= off + n else {
            throw IPCError.payloadTooShort(expected: off + n, got: buf.count)
        }
        let s = n > 0
            ? (String(data: buf.subdata(in: (buf.startIndex + off)..<(buf.startIndex + off + n)), encoding: .utf8) ?? "")
            : ""
        off += n
        return s
    }

    // decodeCombo 读单个按键组合: key(string) + modCount(u32) + modCount×(string)。
    private static func decodeCombo(_ buf: Data, _ off: inout Int) throws -> KeyComboPayload {
        let key = try readLenString(buf, &off)
        guard buf.count >= off + 4 else {
            throw IPCError.payloadTooShort(expected: off + 4, got: buf.count)
        }
        let modCount = Int(buf.readUInt32LE(at: off)); off += 4
        var mods: [String] = []
        mods.reserveCapacity(modCount)
        for _ in 0..<modCount {
            mods.append(try readLenString(buf, &off))
        }
        return KeyComboPayload(key: key, modifiers: mods)
    }

    /// 解码 CmdKeyTap/Hold/Release (0x050E/0x0510/0x0511): 单个 combo。
    public static func decodeKeyComboPayload(_ buf: Data) throws -> KeyComboPayload {
        var off = 0
        return try decodeCombo(buf, &off)
    }

    /// 解码 CmdKeySeq (0x050F): comboCount(u32) + comboCount×combo。
    public static func decodeKeySeqPayload(_ buf: Data) throws -> KeySeqPayload {
        var off = 0
        guard buf.count >= off + 4 else {
            throw IPCError.payloadTooShort(expected: off + 4, got: buf.count)
        }
        let n = Int(buf.readUInt32LE(at: off)); off += 4
        var combos: [KeyComboPayload] = []
        combos.reserveCapacity(n)
        for _ in 0..<n {
            combos.append(try decodeCombo(buf, &off))
        }
        return KeySeqPayload(combos: combos)
    }

    /// 解码 CmdKeyType (0x0512): 整段 UTF-8 文本 (无长度前缀)。
    public static func decodeKeyTypePayload(_ buf: Data) throws -> String {
        return buf.isEmpty ? "" : (String(data: buf, encoding: .utf8) ?? "")
    }

    /// 解 CmdMenuShow (0x0506): count(u32) + count×item; item = id(i32)+flags(u8)
    /// +labelLen(u32)+label+childCount(u32)+children(递归)。flags: 0x01 分隔/0x02 勾选/0x04 禁用。
    public static func decodeUnifiedMenuPayload(_ buf: Data) throws -> [MenuItemData] {
        var off = 0
        let items = try decodeMenuItems(buf, &off)
        return items
    }

    private static func decodeMenuItems(_ buf: Data, _ off: inout Int) throws -> [MenuItemData] {
        guard buf.count >= off + 4 else {
            throw IPCError.payloadTooShort(expected: off + 4, got: buf.count)
        }
        let n = Int(buf.readUInt32LE(at: off)); off += 4
        var out: [MenuItemData] = []
        out.reserveCapacity(n)
        for _ in 0..<n {
            out.append(try decodeMenuItem(buf, &off))
        }
        return out
    }

    private static func decodeMenuItem(_ buf: Data, _ off: inout Int) throws -> MenuItemData {
        guard buf.count >= off + 9 else {
            throw IPCError.payloadTooShort(expected: off + 9, got: buf.count)
        }
        let id = Int32(bitPattern: buf.readUInt32LE(at: off)); off += 4
        let flags = buf[buf.startIndex + off]; off += 1
        let labelLen = Int(buf.readUInt32LE(at: off)); off += 4
        guard buf.count >= off + labelLen else {
            throw IPCError.payloadTooShort(expected: off + labelLen, got: buf.count)
        }
        let label = labelLen > 0
            ? (String(data: buf.subdata(in: (buf.startIndex + off)..<(buf.startIndex + off + labelLen)), encoding: .utf8) ?? "")
            : ""
        off += labelLen
        let children = try decodeMenuItems(buf, &off)
        return MenuItemData(
            id: id, label: label,
            separator: flags & 0x01 != 0, checked: flags & 0x02 != 0, disabled: flags & 0x04 != 0,
            children: children)
    }

    /// 编码 CmdFrontContext (0x0211 upstream): appLen u32 + app + titleLen u32 + title + selLen u32 + sel。
    /// 均 UTF-8、LE 长度前缀。与 Rust `wind-bridge` server.rs 的 CMD_FRONT_CONTEXT 解码对齐。
    public static func encodeFrontContextFrame(app: String, title: String, sel: String) -> Data {
        var payload = Data()
        for s in [app, title, sel] {
            let bytes = Array(s.utf8)
            var lenField = Data(count: 4)
            lenField.writeUInt32LE(UInt32(bytes.count), at: 0)
            payload.append(lenField)
            payload.append(contentsOf: bytes)
        }
        var out = encodeHeader(cmd: UpstreamCmd.frontContext, payloadLen: UInt32(payload.count))
        out.append(payload)
        return out
    }

    /// 解扩展信封 (0x0E01)：`kindLen u32 + kind + bodyLen u32 + body`。
    ///
    /// 低频消息的统一入口，见 Rust `protocol.rs` 的 `CMD_EXT`。**未知 kind 一律安静忽略**
    /// ——这是新旧版本互相兼容的根本，调用方不要把它升级成错误。
    ///
    /// 解不出（截断 / 非法 UTF-8 的 kind）返回 nil，同样按「忽略」处理。
    public static func decodeExt(_ buf: Data) -> (kind: String, body: Data)? {
        guard buf.count >= 4 else { return nil }
        let kindLen = Int(buf.readUInt32LE(at: 0))
        guard buf.count >= 4 + kindLen + 4 else { return nil }
        let base = buf.startIndex
        guard let kind = String(
            data: buf.subdata(in: (base + 4)..<(base + 4 + kindLen)), encoding: .utf8)
        else { return nil }
        let off = 4 + kindLen
        let bodyLen = Int(buf.readUInt32LE(at: off))
        guard buf.count >= off + 4 + bodyLen else { return nil }
        return (kind, buf.subdata(in: (base + off + 4)..<(base + off + 4 + bodyLen)))
    }

    /// 编码 CmdMenuPointer (0x021B 上行)：event u32 + button u32 + x i32 + y i32，共 16 字节。
    /// 移动事件每秒可达上百次，带异步标志、服务端不回 ack，调用方因此无需读响应。
    public static func encodeMenuPointerFrame(event: UInt32, button: UInt32, x: Int32, y: Int32) -> Data {
        var payload = Data(count: 16)
        payload.writeUInt32LE(event, at: 0)
        payload.writeUInt32LE(button, at: 4)
        payload.writeUInt32LE(UInt32(bitPattern: x), at: 8)
        payload.writeUInt32LE(UInt32(bitPattern: y), at: 12)
        var out = encodeHeader(cmd: UpstreamCmd.menuPointer, payloadLen: 16, async: true)
        out.append(payload)
        return out
    }

    /// 编码扩展信封 (0x0E01 上行)。
    public static func encodeExtFrame(kind: String, body: Data) -> Data {
        let kindBytes = Array(kind.utf8)
        var payload = Data(count: 4)
        payload.writeUInt32LE(UInt32(kindBytes.count), at: 0)
        payload.append(contentsOf: kindBytes)
        var lenField = Data(count: 4)
        lenField.writeUInt32LE(UInt32(body.count), at: 0)
        payload.append(lenField)
        payload.append(body)
        var out = encodeHeader(cmd: UpstreamCmd.ext, payloadLen: UInt32(payload.count))
        out.append(payload)
        return out
    }

    /// 解 CmdModeStatus (0x0504): flags(u32)+effectiveMode(u32)+labelLen(u32)+label(UTF-8)。
    /// flags 位: 0x01 中文模式, 0x02 全角, 0x04 中文标点, 0x08 指示器可见, 0x20 CapsLock。
    public static func decodeModeStatusPayload(_ buf: Data) throws -> ModeStatusPayload {
        guard buf.count >= 12 else {
            throw IPCError.payloadTooShort(expected: 12, got: buf.count)
        }
        let flags = buf.readUInt32LE(at: 0)
        let effectiveMode = buf.readUInt32LE(at: 4)
        let labelLen = Int(buf.readUInt32LE(at: 8))
        guard buf.count >= 12 + labelLen else {
            throw IPCError.payloadTooShort(expected: 12 + labelLen, got: buf.count)
        }
        let label = labelLen > 0
            ? (String(data: buf.subdata(in: 12..<(12 + labelLen)), encoding: .utf8) ?? "")
            : ""
        return ModeStatusPayload(
            chineseMode: (flags & 0x0001) != 0,
            fullWidth: (flags & 0x0002) != 0,
            chinesePunct: (flags & 0x0004) != 0,
            capsLock: (flags & 0x0020) != 0,
            visible: (flags & 0x0008) != 0,
            effectiveMode: effectiveMode,
            modeLabel: label)
    }

    /// 解 CmdCandidateRects (0x0503 push): count(u32) + count×(index,x,y,w,h 各 i32 LE)。
    public static func decodeCandidateRectsPayload(_ buf: Data) throws -> [CandidateHitRect] {
        guard buf.count >= 4 else {
            throw IPCError.payloadTooShort(expected: 4, got: buf.count)
        }
        let n = Int(buf.readUInt32LE(at: 0))
        guard buf.count >= 4 + n * 20 else {
            throw IPCError.payloadTooShort(expected: 4 + n * 20, got: buf.count)
        }
        var out: [CandidateHitRect] = []
        out.reserveCapacity(n)
        var off = 4
        for _ in 0..<n {
            out.append(CandidateHitRect(
                index: Int32(bitPattern: buf.readUInt32LE(at: off)),
                x: Int32(bitPattern: buf.readUInt32LE(at: off + 4)),
                y: Int32(bitPattern: buf.readUInt32LE(at: off + 8)),
                w: Int32(bitPattern: buf.readUInt32LE(at: off + 12)),
                h: Int32(bitPattern: buf.readUInt32LE(at: off + 16))))
            off += 20
        }
        return out
    }
}

// MARK: - Data little-endian helpers

extension Data {
    @inline(__always)
    func readUInt16LE(at offset: Int) -> UInt16 {
        let i = self.startIndex + offset
        return UInt16(self[i]) | (UInt16(self[i + 1]) << 8)
    }

    @inline(__always)
    func readUInt32LE(at offset: Int) -> UInt32 {
        let i = self.startIndex + offset
        return UInt32(self[i])
            | (UInt32(self[i + 1]) << 8)
            | (UInt32(self[i + 2]) << 16)
            | (UInt32(self[i + 3]) << 24)
    }

    @inline(__always)
    func readUInt64LE(at offset: Int) -> UInt64 {
        let i = self.startIndex + offset
        var v: UInt64 = 0
        for k in 0..<8 {
            v |= UInt64(self[i + k]) << (8 * k)
        }
        return v
    }

    @inline(__always)
    mutating func writeUInt16LE(_ v: UInt16, at offset: Int) {
        let i = self.startIndex + offset
        self[i]     = UInt8(v & 0xFF)
        self[i + 1] = UInt8((v >> 8) & 0xFF)
    }

    @inline(__always)
    mutating func writeUInt32LE(_ v: UInt32, at offset: Int) {
        let i = self.startIndex + offset
        self[i]     = UInt8(v & 0xFF)
        self[i + 1] = UInt8((v >> 8) & 0xFF)
        self[i + 2] = UInt8((v >> 16) & 0xFF)
        self[i + 3] = UInt8((v >> 24) & 0xFF)
    }
}
