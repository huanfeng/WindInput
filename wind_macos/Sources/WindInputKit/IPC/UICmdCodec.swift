import Foundation

// UICmdCodec — wind_input/internal/uicmd 的 Swift 镜像 (M2.2-A).
//
// wire 布局 (与 Go uicmd/codec.go 完全一致):
//   Command 帧: [cmdType:u16][session:u64][payload bytes...]
//   Event 帧  : [evtType:u16][payload bytes...]
//
// uicmd 帧可作为 bridge `CmdBatchEvents` (0x0F01) 的 payload 一项传输, 也可未来
// 独立通道走 push_pipe. 本端只做 decode (Mac 是接收方); encode 留给未来反向事件
// (CandidateSelect 等) 时补.
//
// 协议同步铁律: 修改任一字段必须三处同步
//   - wind_input/internal/uicmd/*.go (Go SSOT)
//   - wind_tsf/include/... (Win 端)
//   - 本文件 (macOS 端)

// MARK: - 命令类型 (0x06xx 段)

public enum UICmd {
    // 候选框 (0x0601 ~ 0x060F)
    public static let candidatesShow: UInt16     = 0x0601
    public static let candidatesHide: UInt16     = 0x0602
    public static let candidatesPosition: UInt16 = 0x0603
    public static let candidatesMarkers: UInt16  = 0x0604
    public static let candidatesConfig: UInt16   = 0x0605
    public static let candidatesPinState: UInt16 = 0x0606

    // 工具栏 (0x0610 ~ 0x061F)
    public static let toolbarShow: UInt16   = 0x0610
    public static let toolbarHide: UInt16   = 0x0611
    public static let toolbarUpdate: UInt16 = 0x0612

    // 状态/模式指示器 (0x0620 ~ 0x062F)
    public static let statusShow: UInt16   = 0x0620
    public static let statusHide: UInt16   = 0x0621
    public static let statusConfig: UInt16 = 0x0622
    public static let modeShow: UInt16     = 0x0623

    // Tooltip
    public static let tooltipShow: UInt16 = 0x0630
    public static let tooltipHide: UInt16 = 0x0631

    // Toast
    public static let toastShow: UInt16 = 0x0640
    public static let toastHide: UInt16 = 0x0641

    // 菜单
    public static let menuShow: UInt16          = 0x0650
    public static let menuHide: UInt16          = 0x0651
    public static let toolbarMenuHide: UInt16   = 0x0652
    public static let candidateMenuHide: UInt16 = 0x0653

    // 主题/配置
    public static let themeApply: UInt16   = 0x0660
    public static let configUpdate: UInt16 = 0x0661

    // 快捷键
    public static let hotkeysRegister: UInt16   = 0x0670
    public static let hotkeysUnregister: UInt16 = 0x0671

    // 设置/杂项
    public static let settingsOpen: UInt16 = 0x0680
    public static let dpiChanged: UInt16   = 0x0681
}

// MARK: - 事件类型 (0x07xx 段, 上行)

public enum UIEvt {
    public static let candidateSelect: UInt16      = 0x0701
    public static let candidateHover: UInt16       = 0x0702
    public static let candidateContextMenu: UInt16 = 0x0703
    public static let pageUp: UInt16               = 0x0704
    public static let pageDown: UInt16             = 0x0705
    public static let candidateDragEnd: UInt16     = 0x0706
    public static let menuItemSelected: UInt16     = 0x0710
    public static let toolbarClick: UInt16         = 0x0720
    public static let hotkeyTriggered: UInt16      = 0x0730
}

// MARK: - 帧封装 (cmdType + session + payload)

/// 解码后的 uicmd 命令帧元数据 (不含 payload 解析).
public struct UICmdFrame: Equatable {
    public let cmdType: UInt16
    public let session: UInt64
    public let payload: Data        // 剩余字节, 由调用方按 cmdType 进一步解析

    public init(cmdType: UInt16, session: UInt64, payload: Data) {
        self.cmdType = cmdType
        self.session = session
        self.payload = payload
    }
}

public enum UICmdCodec {
    /// 解 uicmd 命令帧的 header (cmdType + session), 剩余字节作为 payload 返回.
    /// 字节长度至少 10 (u16 + u64).
    public static func decodeCommandFrame(_ data: Data) throws -> UICmdFrame {
        guard data.count >= 10 else {
            throw IPCError.payloadTooShort(expected: 10, got: data.count)
        }
        let r = ByteReader(data)
        let cmd = try r.readU16LE()
        let session = try r.readU64LE()
        let payload = data.subdata(in: (data.startIndex + r.position)..<data.endIndex)
        return UICmdFrame(cmdType: cmd, session: session, payload: payload)
    }

    /// 编 uicmd 命令帧的 header. payload 自行追加.
    public static func encodeCommandHeader(cmdType: UInt16, session: UInt64) -> Data {
        let w = ByteWriter(reserving: 10)
        w.writeU16LE(cmdType)
        w.writeU64LE(session)
        return w.bytes()
    }
}

// MARK: - Candidate (镜像 Go uicmd/types.go: Candidate)

public struct Candidate: Equatable {
    public var text: String          // 候选文字
    public var code: String          // 编码 (右键菜单显示)
    public var comment: String       // 注释 (反查编码 / PUA 提示等)
    public var index: Int32          // 显示序号 (1-9/0)
    public var indexLabel: String    // 自定义序号标签, 非空时覆盖 index 数字
    public var source: String        // 候选来源: ""/"codetable"/"pinyin"/"english"/"phrase"
    public var isCommon: Bool        // 通用规范汉字
    public var isPhrase: Bool        // 短语候选
    public var isCommand: Bool       // 命令候选 (uuid/date/time 等)
    public var isGroup: Bool         // 组候选 (展开二级)
    public var isGroupMember: Bool   // 组成员 (右键禁用大部分操作)
    public var hasShadow: Bool       // 存在 Shadow 修改 (右键"恢复默认"用)

    public init(text: String,
                code: String = "",
                comment: String = "",
                index: Int32 = 0,
                indexLabel: String = "",
                source: String = "",
                isCommon: Bool = false,
                isPhrase: Bool = false,
                isCommand: Bool = false,
                isGroup: Bool = false,
                isGroupMember: Bool = false,
                hasShadow: Bool = false) {
        self.text = text
        self.code = code
        self.comment = comment
        self.index = index
        self.indexLabel = indexLabel
        self.source = source
        self.isCommon = isCommon
        self.isPhrase = isPhrase
        self.isCommand = isCommand
        self.isGroup = isGroup
        self.isGroupMember = isGroupMember
        self.hasShadow = hasShadow
    }

    // Flags 位 (与 Go 端 marshalCandidate 对齐):
    static let flagIsCommon: UInt8      = 1 << 0   // 0x01
    static let flagIsPhrase: UInt8      = 1 << 1   // 0x02
    static let flagIsCommand: UInt8     = 1 << 2   // 0x04
    static let flagIsGroup: UInt8       = 1 << 3   // 0x08
    static let flagIsGroupMember: UInt8 = 1 << 4   // 0x10
    static let flagHasShadow: UInt8     = 1 << 5   // 0x20

    /// Wire 编码 (镜像 Go marshalCandidate):
    /// text + code + comment + index:i32 + indexLabel + source + flags:u8
    public func encode(to w: ByteWriter) {
        w.writeString(text)
        w.writeString(code)
        w.writeString(comment)
        w.writeI32LE(index)
        w.writeString(indexLabel)
        w.writeString(source)
        var flags: UInt8 = 0
        if isCommon      { flags |= Candidate.flagIsCommon }
        if isPhrase      { flags |= Candidate.flagIsPhrase }
        if isCommand     { flags |= Candidate.flagIsCommand }
        if isGroup       { flags |= Candidate.flagIsGroup }
        if isGroupMember { flags |= Candidate.flagIsGroupMember }
        if hasShadow     { flags |= Candidate.flagHasShadow }
        w.writeU8(flags)
    }

    public static func decode(from r: ByteReader) throws -> Candidate {
        let text = try r.readString()
        let code = try r.readString()
        let comment = try r.readString()
        let index = try r.readI32LE()
        let indexLabel = try r.readString()
        let source = try r.readString()
        let flags = try r.readU8()
        return Candidate(
            text: text,
            code: code,
            comment: comment,
            index: index,
            indexLabel: indexLabel,
            source: source,
            isCommon:      (flags & flagIsCommon)      != 0,
            isPhrase:      (flags & flagIsPhrase)      != 0,
            isCommand:     (flags & flagIsCommand)     != 0,
            isGroup:       (flags & flagIsGroup)       != 0,
            isGroupMember: (flags & flagIsGroupMember) != 0,
            hasShadow:     (flags & flagHasShadow)     != 0
        )
    }
}

// MARK: - CandidatesShowPayload (cmd 0x0601)

public struct CandidatesShowPayload: Equatable {
    public var candidates: [Candidate]
    public var input: String                 // 拼音/编码原文
    public var cursorPos: Int32              // input 内光标位置 (按 rune)
    public var caretX: Int32                 // 屏幕坐标 (光标点)
    public var caretY: Int32
    public var caretHeight: Int32
    public var page: Int32
    public var totalPages: Int32
    public var totalCandidateCount: Int32
    public var candidatesPerPage: Int32
    public var selectedIndex: Int32          // 当前页内选中候选索引 (0-based)

    public init(candidates: [Candidate] = [],
                input: String = "",
                cursorPos: Int32 = 0,
                caretX: Int32 = 0,
                caretY: Int32 = 0,
                caretHeight: Int32 = 0,
                page: Int32 = 0,
                totalPages: Int32 = 0,
                totalCandidateCount: Int32 = 0,
                candidatesPerPage: Int32 = 0,
                selectedIndex: Int32 = 0) {
        self.candidates = candidates
        self.input = input
        self.cursorPos = cursorPos
        self.caretX = caretX
        self.caretY = caretY
        self.caretHeight = caretHeight
        self.page = page
        self.totalPages = totalPages
        self.totalCandidateCount = totalCandidateCount
        self.candidatesPerPage = candidatesPerPage
        self.selectedIndex = selectedIndex
    }

    /// Wire 编码 (镜像 Go CandidatesShowPayload.marshal):
    /// u32 count + N×Candidate + input + cursorPos + caretX + caretY + caretH
    /// + page + totalPages + totalCount + perPage + selectedIndex (全 i32)
    public func encode(to w: ByteWriter) {
        w.writeU32LE(UInt32(candidates.count))
        for c in candidates { c.encode(to: w) }
        w.writeString(input)
        w.writeI32LE(cursorPos)
        w.writeI32LE(caretX)
        w.writeI32LE(caretY)
        w.writeI32LE(caretHeight)
        w.writeI32LE(page)
        w.writeI32LE(totalPages)
        w.writeI32LE(totalCandidateCount)
        w.writeI32LE(candidatesPerPage)
        w.writeI32LE(selectedIndex)
    }

    public static func decode(from r: ByteReader) throws -> CandidatesShowPayload {
        let n = Int(try r.readU32LE())
        var arr: [Candidate] = []
        arr.reserveCapacity(n)
        for _ in 0..<n {
            arr.append(try Candidate.decode(from: r))
        }
        let input = try r.readString()
        let cursorPos = try r.readI32LE()
        let caretX = try r.readI32LE()
        let caretY = try r.readI32LE()
        let caretH = try r.readI32LE()
        let page = try r.readI32LE()
        let totalPages = try r.readI32LE()
        let totalCount = try r.readI32LE()
        let perPage = try r.readI32LE()
        let selectedIndex = try r.readI32LE()
        return CandidatesShowPayload(
            candidates: arr,
            input: input,
            cursorPos: cursorPos,
            caretX: caretX,
            caretY: caretY,
            caretHeight: caretH,
            page: page,
            totalPages: totalPages,
            totalCandidateCount: totalCount,
            candidatesPerPage: perPage,
            selectedIndex: selectedIndex
        )
    }
}
