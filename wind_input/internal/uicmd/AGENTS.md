<!-- Parent: ../AGENTS.md -->
<!-- Updated: 2026-06-01 -->

# uicmd — UI 命令/事件平台无关数据模型

## Purpose

把"UI 操作"从 `internal/ui` 包中抽出来, 形成 Win/macOS 共享的命令字典。
coordinator 等核心逻辑只产生 `uicmd.Command`, 由不同后端消费:
- Windows: `internal/ui` 包消费命令 → 本地 gg/DirectWrite 渲染
- macOS:   `bridge` forwarder 把命令序列化为协议帧 → IMKit `.app` 自绘 NSPanel 消费

同时定义反向 `uicmd.Event` 类型, 承载渲染端 → Go 服务的上行事件 (鼠标点选、菜单确认、快捷键触发等)。

## Key Files

| File | Description |
|------|-------------|
| `doc.go` | 包级注释, 包含 cmd id 段位分配说明 |
| `command.go` | `Command` 信封 + `CommandType` 枚举 (0x06xx 段) + `Payload` 接口 |
| `event.go` | `Event` 信封 + `EventType` 枚举 (0x07xx 段) + `EventPayload` 接口 |
| `types.go` | `Candidate`/`Color`/`HotkeyEntry` 等公用 wire 类型, 各 string 枚举 (CandidateLayout/ThemeStyle/ToastLevel...) |
| `payload_candidates.go` | 候选框命令 payload + `Candidate` 编解码 helper |
| `payload_toolbar.go` | 工具栏命令 payload + `ToolbarState` 编解码 helper |
| `payload_status.go` | 状态/模式指示器命令 payload + `StatusState` 编解码 helper |
| `payload_tooltip.go` | Tooltip 命令 payload |
| `payload_toast.go` | Toast 命令 payload |
| `payload_menu.go` | 菜单命令 payload + `MenuItem` 递归编解码 |
| `payload_theme.go` | 主题/全局配置 payload + `ThemeColors`/`ThemeFonts`/`ThemeGeometry`/`WindowsRenderHints` 编解码 helper |
| `payload_hotkeys.go` | 全局快捷键注册/撤销 payload |
| `payload_key.go` | 命令直通车按键模拟 payload (0x069x 段): `KeyTapPayload`/`KeyHoldPayload`/`KeyReleasePayload` (单 combo: Key+Modifiers)、`KeySeqPayload` (`[]KeyCombo`)、`KeyTypePayload` (Unicode 文本上屏); 各含 `marshal/unmarshal` + `marshalKeyCombo/unmarshalKeyCombo` helper。darwin 端 forwarder 把这些转译为 ipc `CmdKeyTap`..`CmdKeyType` push 帧, IMKit `.app` 用 CGEvent/insertText 执行 |
| `event_payloads.go` | 所有上行事件 payload |
| `codec.go` | `EncodeCommand`/`DecodeCommand`/`EncodeEvent`/`DecodeEvent` 主入口与 type→payload dispatch |
| `codec_buffer.go` | 私有 `binWriter`/`binReader`, 小端字节流读写工具 |
| `codec_test.go` | 所有命令/事件 roundtrip 测试 + 错误路径测试 |

## For AI Agents

### Working In This Directory

- **零依赖 internal/ui**: 本包是 ui 的上游, 不允许反向 import。如果遇到字段从 `internal/ui` 类型取的需求, 必须在本包定义镜像类型 (见 `ToolbarState`、`StatusState` 的处理)。
- **新增命令的流程** (5 步):
  1. 在 `command.go` 加 `CmdXxx CommandType = 0x06XX` (按段位选 id), 同步更新 `commandNames`
  2. 在合适的 `payload_*.go` 文件定义 `XxxPayload` struct + `isPayload()/CommandType()` 实现
  3. 在同一文件追加 `marshal(*binWriter)` (value receiver) 与 `unmarshal(*binReader)` (pointer receiver)
  4. 在 `codec.go` 的 `marshalPayload`/`unmarshalPayload` switch 加分支
  5. 在 `codec_test.go` 加 roundtrip 测试 (至少一个含非零字段的"非平凡"值)
- **wire 布局约定** (与 `internal/ipc/binary_codec.go` 风格一致, 便于 IMKit/wind_tsf 端对称解码):
  - LittleEndian
  - 字符串: `uint32 长度 + UTF-8 bytes`
  - 切片: `uint32 count + 元素逐个`
  - map: `uint32 count + (key, value) 对`
  - nullable: `uint8 present (0/1) + 内容?` (见 `writeOptColor`)
- **payload 字段类型选择**: 物理尺寸/坐标用 `int` (Go 端常见), 协议层用 `int32` 编码; 布尔型用 `bool` (序列化为 1 字节)。
- **CommandType 与 Payload 必须匹配**: `NewCommand` 会断言, `EncodeCommand` 会校验, 编程错误在开发期暴露。
- **MenuShowPayload.SessionID 是 callback 路由 key**: Go 服务端发 menu 时分配 sessionID, 上行 `EvtMenuItemSelected` 携带同一 sessionID 用于找回当时登记的 callback。**不要**在 payload 直接传函数指针。
- **`WindowsRenderHints` 是 Win 渲染后端专有**: darwin 端反序列化后忽略该结构。新增 Win 专有渲染参数追加到此结构, 不要污染顶层 payload 字段。

### Testing Requirements

- `go test ./internal/uicmd/...` 必须全绿。
- 新增 payload 必须在 `codec_test.go` 加 roundtrip 测试; 测试值需覆盖:
  - 含中文字符串
  - 切片/map 含 0 项与 N 项两种
  - nullable 字段含 nil 与非 nil 两种
  - 负数坐标 (验证 int32 与 int 转换正确)
- 错误路径覆盖: 未知 cmd id、buffer underflow、payload 与 type 不匹配。

### Common Patterns

- **生产命令**:
  ```go
  cmd := uicmd.NewCommand(uicmd.CmdCandidatesShow, sessionID,
      uicmd.CandidatesShowPayload{ /* 字段 */ })
  ```
- **序列化** (macOS forwarder 用): `buf, err := uicmd.EncodeCommand(cmd)`
- **反序列化** (IMKit 端对称实现): `cmd, err := uicmd.DecodeCommand(buf)`
- **添加可选字段保兼容**: payload struct 字段尾部追加 + 反序列化端判 `r.eof()` 是否还有数据。**不要**在已有字段中间插入。

## Dependencies

### Internal

- 无 (本包是 ui 上游的"叶子" wire 数据包, 不依赖项目内任何其他包)

### External

- 仅 `encoding/binary`、`errors`、`fmt`、`math`、`reflect` (测试用) 标准库

## 全局约束

- 枚举与魔法字符串约束: 见 [`/docs/design/enum-constraint.md`](../../../docs/design/enum-constraint.md)。本包定义的 string 枚举 (`CandidateLayout`、`ThemeStyle` 等) 是 `pkg/config` 中对应枚举的"wire 镜像", 取值字面值必须与原始 SSOT 对齐, 不可单边修改。
- 日志隐私: 本包不输出日志; 调用方序列化命令时如需 DEBUG 打印应避免泄露 `Candidate.Text` / `Input` 等用户内容到 INFO 及以下级别。

<!-- MANUAL: Any manually added notes below this line are preserved on regeneration -->
