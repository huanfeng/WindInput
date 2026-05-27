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
