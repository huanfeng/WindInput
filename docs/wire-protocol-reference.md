<!-- Updated: 2026-05-26 -->

# Wire 协议速查 (IPC + UICmd)

> 为 macOS IMKit `.app` (Swift/Obj-C) 端写解码器的查表式参考.
> Win 端 C++ 实现可对照 `wind_tsf/src/IPCClient.cpp` 与 `wind_tsf/include/BinaryProtocol.h`.

## 0. 协议总览

WindInput 跨进程 IPC 共有两套**字节布局兼容**的协议:

1. **bridge IPC** (`internal/ipc/binary_protocol.go`): IME 客户端 ↔ Go 服务的核心通道, 处理键事件/焦点/状态/Commit/Composition
2. **uicmd** (`internal/uicmd/`): Go → 渲染端的 UI 命令 + 渲染端 → Go 的 UI 事件

两套协议**帧布局相同** (uint16 cmd + uint32 length 等), uicmd 帧可作为 bridge `CmdBatchEvents` (0x0F01) 的 payload 传输, 或后续 PR 内独立打包.

## 1. 帧格式

### 1.1 Header (固定 8 字节)

```
┌─────────┬──────┬──────┬──────────────────────┐
│ 偏移    │ 长度 │ 字段 │ 说明                  │
├─────────┼──────┼──────┼──────────────────────┤
│ 0       │ 2    │ Ver  │ LittleEndian uint16   │
│ 2       │ 2    │ Cmd  │ LittleEndian uint16   │
│ 4       │ 4    │ Len  │ LittleEndian uint32   │
└─────────┴──────┴──────┴──────────────────────┘
```

- `Ver` = `0x1001` (主版本 0x1 + 子版本 0x001). 高 4 位是 major version, 客户端只需校验 major. 高位 `0x8000` 是 `AsyncFlag` (异步请求, 不需要响应)
- `Cmd` = 命令 ID, 见 §2 / §3 / §4
- `Len` = payload 字节数, 最大 1 MB (`MaxPayloadSize = 1024*1024`)

### 1.2 通用编码约定

| 类型 | 字节布局 |
|------|---------|
| `uint8` | 1 字节 |
| `bool` | 1 字节 (0 = false, 非 0 = true) |
| `uint16` / `int16` | 2 字节, LittleEndian |
| `uint32` / `int32` | 4 字节, LittleEndian |
| `uint64` | 8 字节, LittleEndian |
| `float64` | 8 字节, IEEE 754 LittleEndian (Go 用 `math.Float64bits`) |
| `string` | `uint32 len` + `len` 字节 UTF-8 |
| `bytes` | `uint32 len` + `len` 字节原始 |
| `[]T` (切片) | `uint32 count` + `count` 个 T 元素 |
| `map[K]V` | `uint32 count` + `count` 对 (K, V) 平铺 |
| `*T` (nullable) | `uint8 present` (0/1) + present=1 时跟 T |
| `Color` (RGBA) | 4 个 uint8: R, G, B, A |

## 2. bridge 上行 (客户端 → Go 服务)

| Cmd ID | 名称 | Payload 布局 | 说明 |
|--------|------|--------------|------|
| `0x0101` | CmdKeyEvent | `keyCode:u32 + scanCode:u32 + modifiers:u32 + eventType:u8 + toggles:u8 + eventSeq:u16` (+ 可选 `prevChar:u16` 扩展到 18 字节) | 键事件; eventType: 0=down, 1=up |
| `0x0104` | CmdCommitRequest | `barrierSeq:u16 + triggerKey:u16 + modifiers:u32 + inputLength:u32 + inputBuffer:bytes(inputLength)` | barrier 提交: 空格/Enter/数字选词 |
| `0x0201` | CmdFocusGained | `processID:u32` (+ 可选 token 扩展) | 文本框获焦 |
| `0x0202` | CmdFocusLost | (空) | 文本框失焦 |
| `0x0203` | CmdIMEActivated | `processID:u32` (+ 可选 token) | 用户切到本 IME |
| `0x0204` | CmdIMEDeactivated | (空) | 用户切到其它 IME |
| `0x0205` | CmdModeNotify | `chineseMode:bool + clearInput:bool` | TSF 端本地切换模式 (异步) |
| `0x0207` | CmdToggleMode | (空) | 请求切换中英模式 |
| `0x020A` | CmdShowContextMenu | `screenX:i32 + screenY:i32` | 客户端请求 Go 弹右键菜单 |
| `0x020B` | CmdSystemModeSwitch | `chineseMode:bool` | 系统级 Ctrl+Space 强制目标模式 |
| `0x0301` | CmdCaretUpdate | `x:i32 + y:i32 + height:i32` (+ 可选 `compositionStartX:i32 + compositionStartY:i32` 扩展到 20 字节) | 光标位置更新 |
| `0x0302` | CmdSelectionChanged | (空 或 `prevChar:u16`) | 光标移到非 composition 区域 |
| `0x0303` | CmdCaretPending | (空) | composition 启动, 真正 caret 后到达 |
| `0x0F01` | CmdBatchEvents | `eventCount:u16 + reserved:u16 + N×(header + payload)` | 批量帧容器 (减少 syscall) |
| `0x0F03` | CmdInputStats | (TBD) | 英文模式输入统计 (异步) |

## 3. bridge 下行 (Go 服务 → 客户端)

主请求-响应通道返回:

| Cmd ID | 名称 | Payload | 说明 |
|--------|------|---------|------|
| `0x0001` | CmdAck | (空) | 通用 ACK |
| `0x0002` | CmdPassThrough | (空) | 按键未消费, 让系统处理 |
| `0x0401` | CmdConsumed | (空) | 按键已消费, 不出字 |
| `0x0101` | CmdCommitText | `flags:u32 + textLen:u32 + compLen:u32 + text:bytes + composition:bytes` | flags: 0x01=ModeChanged, 0x02=HasNewComposition, 0x04=ChineseMode |
| `0x0102` | CmdUpdateComposition | `caretPos:u32 + text:bytes(剩余)` | 更新 preedit |
| `0x0103` | CmdClearComposition | (空) | 清除 preedit |
| `0x0105` | CmdCommitResult | `barrierSeq:u16 + flags:u16 + textLen:u32 + compLen:u32 + text:bytes + composition:bytes` | 对应上行 CmdCommitRequest 的应答; flags 含 CommitFlagModeChanged/HasNewComposition/ChineseMode |
| `0x0106` | CmdCommitTextWithCursor | `textLen:u32 + cursorOffset:u32 + text:bytes` | 上屏 + 把光标向左偏移 |
| `0x0107` | CmdMoveCursor | `direction:u32` (1=right) | 智能跳过 |
| `0x0108` | CmdDeletePair | (空) | 配对删除: 删左1+右1 |

push 通道单向:

| Cmd ID | 名称 | Payload |
|--------|------|---------|
| `0x0202` | CmdStatusUpdate | `flags:u32 + keyDownCount:u32 + keyUpCount:u32 + N×u32 hotkeys + iconLabel:bytes(剩余)` |
| `0x0206` | CmdStatePush | `flags:u32 + 0:u32 + 0:u32 + iconLabel:bytes(剩余)` |
| `0x0207` | CmdServiceReady | (空) — push 客户端连接后, 提示其请求全量状态 |
| `0x0301` | CmdSyncHotkeys | `0:u32 + keyDownCount:u32 + keyUpCount:u32 + N×u32 hotkeys` |
| `0x0303` | CmdSyncConfig | `keyLen:u16 + valueLen:u32 + key:bytes + value:bytes` |
| `0x0501` | CmdHostRenderSetup | (Win 专有, darwin 不发) |
| `0x0F02` | CmdBatchResponse | `count:u16 + reserved:u16 + N×response` |

`StatusUpdate` flags 位:
| Bit | 含义 |
|-----|------|
| 0x01 | `StatusChineseMode` |
| 0x02 | `StatusFullWidth` |
| 0x04 | `StatusChinesePunct` |
| 0x08 | `StatusToolbarVisible` |
| 0x10 | `StatusCapsLock` |
| 0x20 | `StatusHostRenderAvail` (Win 专有) |

## 4. uicmd 命令 (Go → 渲染端, 0x06xx 段)

uicmd 帧自身布局 (作为 bridge `CmdBatchEvents` 的一项, 或未来独立通道):
```
cmdType:u16 + session:u64 + payload bytes
```

`session` 用于 stale 检测 (旧 show 命令被新 hide 覆盖时丢弃).

### 4.1 候选框

| Cmd | 名称 | Payload |
|-----|------|---------|
| `0x0601` | CmdCandidatesShow | `count:u32 + N×Candidate + input:string + cursorPos:i32 + caretX:i32 + caretY:i32 + caretH:i32 + page:i32 + totalPages:i32 + totalCount:i32 + perPage:i32 + selectedIndex:i32` |
| `0x0602` | CmdCandidatesHide | (空) |
| `0x0603` | CmdCandidatesPosition | `x:i32 + y:i32` |
| `0x0604` | CmdCandidatesMarkers | `isPinyin:bool + isQuickInput:bool + modeLabel:string + accentColor:OptColor` |
| `0x0605` | CmdCandidatesConfig | `layout:string + hideCandWin:bool + hidePreedit:bool + preeditMode:string + pagerMode:string + cmdbarPrefix:string + maxCandidateChars:i32 + fontSize:f64 + fontFamily:string` |
| `0x0606` | CmdCandidatesPinState | `enabled:bool + count:u32 + N×(monitorKey:string + x:i32 + y:i32)` |

**Candidate** 单元布局 (`internal/uicmd/payload_candidates.go: marshalCandidate`):
```
text:string + code:string + comment:string + index:i32 + indexLabel:string + source:string + flags:u8
```
flags 位: `0x01=IsCommon, 0x02=IsPhrase, 0x04=IsCommand, 0x08=IsGroup, 0x10=IsGroupMember, 0x20=HasShadow`

### 4.2 工具栏

| Cmd | 名称 | Payload |
|-----|------|---------|
| `0x0610` | CmdToolbarShow | `x:i32 + y:i32 + ToolbarState` |
| `0x0611` | CmdToolbarHide | (空) |
| `0x0612` | CmdToolbarUpdate | `ToolbarState` |

**ToolbarState** (`internal/uicmd/payload_toolbar.go: writeToolbarState`):
```
chineseMode:bool + capsLock:bool + fullWidth:bool + chinesePunct:bool + effectiveMode:i32 + modeLabel:string
```

### 4.3 状态指示器 / 模式浮窗

| Cmd | 名称 | Payload |
|-----|------|---------|
| `0x0620` | CmdStatusShow | `StatusState + x:i32 + y:i32` |
| `0x0621` | CmdStatusHide | (空) |
| `0x0622` | CmdStatusConfig | `enabled:bool + displayMode:string + duration:i32 + schemaNameStyle:string + showMode:bool + showPunct:bool + showFullWidth:bool + positionMode:string + offsetX:i32 + offsetY:i32 + customX:i32 + customY:i32 + fontSize:f64 + opacity:f64 + bgColor:string + textColor:string + borderRadius:f64` |
| `0x0623` | CmdModeShow | `mode:string + x:i32 + y:i32` |

**StatusState**: `modeLabel:string + punctLabel:string + widthLabel:string`

### 4.4 Tooltip

| Cmd | 名称 | Payload |
|-----|------|---------|
| `0x0630` | CmdTooltipShow | `text:string + centerX:i32 + belowY:i32 + aboveY:i32` |
| `0x0631` | CmdTooltipHide | (空) |

### 4.5 Toast

| Cmd | 名称 | Payload |
|-----|------|---------|
| `0x0640` | CmdToastShow | `title:string + message:string + level:string + position:string + duration:i32 + maxWidth:i32` |
| `0x0641` | CmdToastHide | (空) |

`level` 取值: `"info"` / `"success"` / `"warn"` / `"error"`
`position` 取值: `"center"` / `"bottom_right"`

### 4.6 菜单

| Cmd | 名称 | Payload |
|-----|------|---------|
| `0x0650` | CmdMenuShow | `sessionID:u64 + screenX:i32 + screenY:i32 + flipRefY:i32 + count:u32 + N×MenuItem` |
| `0x0651` | CmdMenuHide | (空) |
| `0x0652` | CmdToolbarMenuHide | (空) |
| `0x0653` | CmdCandidateMenuHide | (空) |

**MenuItem (uicmd 版)** (`internal/uicmd/payload_menu.go: writeMenuItem`, 递归):
```
id:i32 + label:string + type:string + checked:bool + disabled:bool + childCount:u32 + N×MenuItem
```
`type` 取值: `"normal"` / `"separator"` / `"checkable"` / `"radio"`

### 4.7 主题 / 配置

| Cmd | 名称 | Payload |
|-----|------|---------|
| `0x0660` | CmdThemeApply | `themeID:string + style:string + ThemeColors + ThemeFonts + ThemeGeometry + WindowsRenderHints` |
| `0x0661` | CmdConfigUpdate | `fontSize:f64 + fontFamily:string + tooltipDelay:i32 + darkMode:bool + WindowsRenderHints` |

**ThemeColors** (13 个 Color, 顺序固定):
```
background, border, text, textSelected, textComment, highlightBg,
indexNormal, indexSelected, statusBg, statusText,
toastInfoAccent, toastWarnAccent, toastErrorAccent
```

**ThemeFonts** (6 个 `family:string + size:f64` 对, 顺序):
```
(family, size), (commentFamily, commentSize), (indexFamily, indexSize),
(menuFamily, menuSize), (statusFamily, statusSize), (toastFamily, toastSize)
```

**ThemeGeometry** (8 个 f64, 顺序):
```
borderRadius, borderWidth, paddingX, paddingY,
itemSpacing, shadowRadius, shadowOffset, opacity
```

**WindowsRenderHints** (Win 专有, darwin 端忽略):
```
textRenderMode:string + gdiFontWeight:i32 + gdiFontScale:f64
+ menuFontWeight:i32 + menuFontScale:f64 + menuFontSize:f64
```

### 4.8 快捷键

| Cmd | 名称 | Payload |
|-----|------|---------|
| `0x0670` | CmdHotkeysRegister | `count:u32 + N×HotkeyEntry` |
| `0x0671` | CmdHotkeysUnregister | (空) |

**HotkeyEntry**:
```
id:i32 + mods:u32 + keyCode:u32 + command:string
```

### 4.9 其他

| Cmd | 名称 | Payload |
|-----|------|---------|
| `0x0680` | CmdSettingsOpen | `page:string` |
| `0x0681` | CmdDPIChanged | (空) — Win 专有, darwin 端忽略 |

## 5. uicmd 事件 (渲染端 → Go, 0x07xx 段)

事件帧布局: `evtType:u16 + payload bytes`

| Evt | 名称 | Payload |
|-----|------|---------|
| `0x0701` | EvtCandidateSelect | `index:i32` |
| `0x0702` | EvtCandidateHover | `index:i32 + tooltipX:i32 + belowY:i32 + aboveY:i32` |
| `0x0703` | EvtCandidateContextMenu | `index:i32 + action:string` |
| `0x0704` | EvtPageUp | (空) |
| `0x0705` | EvtPageDown | (空) |
| `0x0706` | EvtCandidateDragEnd | `x:i32 + y:i32` |
| `0x0710` | EvtMenuItemSelected | `sessionID:u64 + itemID:i32` |
| `0x0720` | EvtToolbarClick | `action:string + x:i32 + y:i32` |
| `0x0730` | EvtHotkeyTriggered | `command:string` |

### EvtCandidateContextMenu.action 取值
`"move_up"` / `"move_down"` / `"move_top"` / `"delete"` / `"reset_default"` / `"copy"` / `"copy_debug_batch"` / `"open_settings"` / `"about"` / `"show_unified_menu"`

### EvtToolbarClick.action 取值
`"toggle_mode"` / `"toggle_width"` / `"toggle_punct"` / `"open_menu"` / `"drag_end"` / `"open_settings"` / `"context_settings"` / `"context_restart"` / `"context_about"`

## 6. 字符串枚举完整清单

下表是字段值为 string 的常量取值, 实现端必须按字面值匹配:

| 类别 | 取值 |
|------|------|
| `CandidateLayout` | `"horizontal"` / `"vertical"` |
| `PreeditMode` | (镜像 `pkg/config.PreeditMode`, e.g. `"top"`/`"embedded"`/`"inline"`) |
| `PagerDisplayMode` | (镜像 `pkg/config.PagerDisplayMode`, e.g. `"never"`/`"auto"`/`"always"`/`""`) |
| `ThemeStyle` | `"system"` / `"light"` / `"dark"` |
| `ToastLevel` | `"info"` / `"success"` / `"warn"` / `"error"` |
| `ToastPosition` | `"center"` / `"bottom_right"` |
| `StatusDisplayMode` | (镜像 `internal/ui.StatusDisplayMode`, e.g. `"temp"`/`"always"`) |
| `StatusPositionMode` | (镜像 `internal/ui.StatusPositionMode`, e.g. `"follow_caret"`/`"custom"`) |
| `CandidateContextMenuAction` | 见 §5 |
| `ToolbarClickAction` | 见 §5 |

## 7. 版本扩展约定

- 字段**只能向尾部追加**: 旧客户端读到自己不认识的尾部字节时, 应直接忽略
- **不能修改字段顺序或类型**: 这会破坏现有 wire 兼容性
- 新增 cmd 用未占用 id (如 0x06xx 段还有大量空闲)
- protocol version (header Ver 字段) 当前 0x1001; 整体不兼容变更时 bump major (0x2000)

## 8. 实现速查 (Swift)

最小协议解码器骨架:

```swift
struct Frame {
    let cmd: UInt16
    let payload: Data
}

extension InputStream {
    func readU16LE() throws -> UInt16 {
        var b = [UInt8](repeating: 0, count: 2)
        guard read(&b, maxLength: 2) == 2 else { throw IPCError.eof }
        return UInt16(b[0]) | (UInt16(b[1]) << 8)
    }
    func readU32LE() throws -> UInt32 {
        var b = [UInt8](repeating: 0, count: 4)
        guard read(&b, maxLength: 4) == 4 else { throw IPCError.eof }
        return UInt32(b[0]) | (UInt32(b[1]) << 8) | (UInt32(b[2]) << 16) | (UInt32(b[3]) << 24)
    }
    func readU64LE() throws -> UInt64 {
        var b = [UInt8](repeating: 0, count: 8)
        guard read(&b, maxLength: 8) == 8 else { throw IPCError.eof }
        var v: UInt64 = 0
        for i in 0..<8 { v |= UInt64(b[i]) << (8 * i) }
        return v
    }
    func readBytes(_ n: Int) throws -> Data {
        var data = Data(count: n)
        let got = data.withUnsafeMutableBytes { read($0.bindMemory(to: UInt8.self).baseAddress!, maxLength: n) }
        guard got == n else { throw IPCError.eof }
        return data
    }
    func readString() throws -> String {
        let n = Int(try readU32LE())
        guard n > 0 else { return "" }
        let bytes = try readBytes(n)
        return String(data: bytes, encoding: .utf8) ?? ""
    }
    func readBool() throws -> Bool {
        var b: UInt8 = 0
        guard read(&b, maxLength: 1) == 1 else { throw IPCError.eof }
        return b != 0
    }
}

func readFrame(_ s: InputStream) throws -> Frame {
    let ver = try s.readU16LE()
    let cmd = try s.readU16LE()
    let len = try s.readU32LE()
    guard ver & 0xF000 == 0x1000 else { throw IPCError.versionMismatch }
    let payload = len > 0 ? try s.readBytes(Int(len)) : Data()
    return Frame(cmd: cmd, payload: payload)
}
```

## 9. 协议生成工具 (建议)

未来可以从 Go 源码 (`internal/uicmd/*.go`) 自动生成 Swift `ProtocolTypes.swift` 与 `BinaryCodec.swift`, 避免手工同步漂移. 暂时手工维护, 但**修改协议时务必三处同步**:
- `wind_input/internal/uicmd/*.go` (Go SSOT)
- `wind_input/internal/uicmd/codec_test.go` (roundtrip 测试)
- `wind_macos/Sources/IPC/ProtocolTypes.swift` (待 PR-A 创建)

## 10. 调试小工具

仓库根可考虑加 `tools/proto_dump/main.go`, 接收一段 hex bytes 输出解析后的命令/事件结构 — 帮助 Swift 端 implementer 在不跑全工程的情况下验证编码正确性. (留作 PR-A.1 工作项.)
