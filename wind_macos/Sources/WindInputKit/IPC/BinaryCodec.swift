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

    /// 解 CmdMenuShow (0x0506): count(u32) + count×item; item = id(i32)+flags(u8)
    /// +labelLen(u32)+label+childCount(u32)+children(递归)。flags: 0x01 分隔/0x02 勾选/0x04 禁用。
    /// 解码 CmdTooltipShow (0x0508): textLen+text + bgLen+bg + fgLen+fg, 均 UTF-8。
    public static func decodeTooltipPayload(_ buf: Data) throws -> TooltipPayload {
        var off = 0
        func readStr() throws -> String {
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
        let text = try readStr()
        let bg = try readStr()
        let fg = try readStr()
        // fontPath 为后加字段; off 已到末尾 (旧服务无此段) 时容忍缺省为空。
        let fontPath = off < buf.count ? try readStr() : ""
        return TooltipPayload(text: text, bgColor: bg, fgColor: fg, fontPath: fontPath)
    }

    public static func decodeStatusBubblePayload(_ buf: Data) throws -> StatusBubblePayload {
        var off = 0
        func readStr() throws -> String {
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
        let text = try readStr()
        let bg = try readStr()
        let fg = try readStr()
        guard buf.count >= off + 12 else {
            throw IPCError.payloadTooShort(expected: off + 12, got: buf.count)
        }
        let x = Int32(bitPattern: buf.readUInt32LE(at: off)); off += 4
        let y = Int32(bitPattern: buf.readUInt32LE(at: off)); off += 4
        let dur = Int32(bitPattern: buf.readUInt32LE(at: off))
        return StatusBubblePayload(text: text, bgColor: bg, fgColor: fg, x: x, y: y, durationMs: dur)
    }

    public static func decodeToastPayload(_ buf: Data) throws -> ToastPayload {
        var off = 0
        func readStr() throws -> String {
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
        let title = try readStr()
        let message = try readStr()
        let bg = try readStr()
        let fg = try readStr()
        let accent = try readStr()
        let position = try readStr()
        guard buf.count >= off + 8 else {
            throw IPCError.payloadTooShort(expected: off + 8, got: buf.count)
        }
        let dur = Int32(bitPattern: buf.readUInt32LE(at: off)); off += 4
        let maxWidth = Int32(bitPattern: buf.readUInt32LE(at: off))
        return ToastPayload(title: title, message: message, bgColor: bg, fgColor: fg,
                            accentColor: accent, position: position, durationMs: dur, maxWidth: maxWidth)
    }

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

    /// 编码 CmdCandidateContextMenu (0x020F upstream): index i32 + actionLen u32 + action UTF-8。
    public static func encodeCandidateContextMenuFrame(index: Int, action: String) -> Data {
        let actionBytes = Array(action.utf8)
        var payload = Data(count: 8)
        payload.writeUInt32LE(UInt32(bitPattern: Int32(index)), at: 0)
        payload.writeUInt32LE(UInt32(actionBytes.count), at: 4)
        payload.append(contentsOf: actionBytes)
        var out = encodeHeader(cmd: UpstreamCmd.candidateContextMenu, payloadLen: UInt32(payload.count))
        out.append(payload)
        return out
    }

    /// 解 CmdCandidateMenuFlags (0x0505): count(u32) + count×(1 字节禁用位)。
    /// 禁用位: 0x01 上移, 0x02 下移, 0x04 置顶, 0x08 删除, 0x10 恢复默认。
    public static func decodeCandidateMenuFlagsPayload(_ buf: Data) throws -> [UInt8] {
        guard buf.count >= 4 else {
            throw IPCError.payloadTooShort(expected: 4, got: buf.count)
        }
        let n = Int(buf.readUInt32LE(at: 0))
        guard buf.count >= 4 + n else {
            throw IPCError.payloadTooShort(expected: 4 + n, got: buf.count)
        }
        return [UInt8](buf.subdata(in: 4..<(4 + n)))
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
