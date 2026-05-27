import Foundation

// ByteWriter / ByteReader — 小端字节流读写工具, 镜像 Go uicmd/codec_buffer.go 的
// binWriter / binReader. 字符串/切片统一用 uint32 长度前缀.
//
// 与 BinaryCodec.swift 里的 Data extension (readUInt16LE 等定位访问) 不同, 这一对
// 是 *流式* 读写, 维护内部 position 自增, 用于嵌套结构的递归编解码 (Candidate /
// Color / 嵌套数组等).

// MARK: - Errors

extension IPCError {
    /// 读到尾部但字段还没读完 (镜像 errBufUnderflow).
    public static var bufferUnderflow: IPCError { .readFailed("buffer underflow") }
}

// MARK: - ByteWriter

public final class ByteWriter {
    public private(set) var data: Data

    public init(reserving cap: Int = 64) {
        self.data = Data()
        self.data.reserveCapacity(cap)
    }

    public func bytes() -> Data { data }

    public func writeU8(_ v: UInt8) {
        data.append(v)
    }

    public func writeBool(_ v: Bool) {
        data.append(v ? 1 : 0)
    }

    public func writeU16LE(_ v: UInt16) {
        data.append(UInt8(v & 0xFF))
        data.append(UInt8((v >> 8) & 0xFF))
    }

    public func writeU32LE(_ v: UInt32) {
        data.append(UInt8(v & 0xFF))
        data.append(UInt8((v >> 8) & 0xFF))
        data.append(UInt8((v >> 16) & 0xFF))
        data.append(UInt8((v >> 24) & 0xFF))
    }

    public func writeI32LE(_ v: Int32) {
        writeU32LE(UInt32(bitPattern: v))
    }

    public func writeU64LE(_ v: UInt64) {
        for k in 0..<8 {
            data.append(UInt8((v >> (8 * k)) & 0xFF))
        }
    }

    public func writeF64(_ v: Double) {
        writeU64LE(v.bitPattern)
    }

    /// 字符串: uint32 len + UTF-8 bytes. 与 Go writeString 完全一致.
    public func writeString(_ s: String) {
        let utf8 = Array(s.utf8)
        writeU32LE(UInt32(utf8.count))
        data.append(contentsOf: utf8)
    }
}

// MARK: - ByteReader

public final class ByteReader {
    public let data: Data
    public private(set) var position: Int

    public init(_ data: Data) {
        self.data = data
        self.position = data.startIndex
    }

    public var remaining: Int { data.endIndex - position }
    public var isEOF: Bool { position >= data.endIndex }

    private func require(_ n: Int) throws {
        guard remaining >= n else { throw IPCError.bufferUnderflow }
    }

    public func readU8() throws -> UInt8 {
        try require(1)
        let v = data[position]
        position += 1
        return v
    }

    public func readBool() throws -> Bool {
        return try readU8() != 0
    }

    public func readU16LE() throws -> UInt16 {
        try require(2)
        let v = UInt16(data[position]) | (UInt16(data[position + 1]) << 8)
        position += 2
        return v
    }

    public func readU32LE() throws -> UInt32 {
        try require(4)
        let v = UInt32(data[position])
            | (UInt32(data[position + 1]) << 8)
            | (UInt32(data[position + 2]) << 16)
            | (UInt32(data[position + 3]) << 24)
        position += 4
        return v
    }

    public func readI32LE() throws -> Int32 {
        return Int32(bitPattern: try readU32LE())
    }

    public func readU64LE() throws -> UInt64 {
        try require(8)
        var v: UInt64 = 0
        for k in 0..<8 {
            v |= UInt64(data[position + k]) << (8 * k)
        }
        position += 8
        return v
    }

    public func readF64() throws -> Double {
        return Double(bitPattern: try readU64LE())
    }

    /// 字符串: uint32 len + UTF-8 bytes.
    public func readString() throws -> String {
        let n = Int(try readU32LE())
        guard n >= 0 else { throw IPCError.bufferUnderflow }
        if n == 0 { return "" }
        try require(n)
        let slice = data.subdata(in: position..<(position + n))
        position += n
        return String(data: slice, encoding: .utf8) ?? ""
    }
}
