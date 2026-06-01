<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-06-01 -->

# internal/ipc

## Purpose
底层 IPC 基础设施。定义二进制通信协议（命令码、消息头、编解码器）和基础 Named Pipe 服务端框架。`bridge` 包在此之上构建业务逻辑。

注意：`server.go` 中还保留了早期的 JSON 协议服务端（`\\.\pipe\tsf_ime_service`），当前主服务已迁移到 `bridge` 包的二进制协议，此文件为遗留代码。

### 跨语言协议同步（必读）
本目录的二进制协议与 C++ 侧 [`wind_tsf/include/BinaryProtocol.h`](../../../wind_tsf/include/BinaryProtocol.h) 互为镜像，由 [`wind_tsf/src/IPCClient.cpp`](../../../wind_tsf/src/IPCClient.cpp) 在 C++ 端实现编解码。**修改 `binary_protocol.go` / `binary_codec.go` 中的命令码、Header 字段、Payload 结构、状态标志位时，必须同步修改 `BinaryProtocol.h` 与 `IPCClient.cpp`，否则会破坏 IPC 兼容性。** C++ 侧的概览见 [`/wind_tsf/AGENTS.md`](../../../wind_tsf/AGENTS.md)。

## Key Files
| File | Description |
|------|-------------|
| `protocol.go` | JSON 协议类型（RequestType、Request、Response、Candidate）— 遗留 |
| `binary_protocol.go` | 二进制协议命令码常量（上行 `CmdKeyEvent`/`CmdFocusGained`/`CmdCandidateSelect` 等，下行 `CmdCommitText`/`CmdStatusUpdate`/`CmdActivationStatusPush`/`CmdHostRenderSetup`/`CmdHostRenderFrame`/`CmdCandidateRects` 等）；消息头/载荷结构体（`IpcHeader`、`KeyPayload`、`CaretPayload`、`HostRenderFramePayload`、`CandidateHitRect` 等）；共享内存协议常量（`SharedRenderMagic`、`SharedRenderHeaderSize`、`MaxSharedRenderSize`、`SharedFlagVisible`/`SharedFlagContentReady`）；`SharedRenderHeader`、`HostRenderSetupPayload` 结构体；`StatusHostRenderAvail` 状态标志位 |
| `binary_codec.go` | `BinaryCodec`：消息的二进制编解码；`EncodeStatusUpdateEx`（含 `hostRenderAvail`）、`EncodeActivationStatusPush`（IMEActivated/FocusGained 异步化状态回包，含 hotkeys + hostRenderAvail + iconLabel）、`EncodeHostRenderSetup`、`EncodeHostRenderFrame`（darwin SHM 新帧就绪通知, 24B: seq+x+y+w+h+flags）、`EncodeBatchResponse`/`DecodeBatchEvents`、`EncodeStatePush`；`EncodeKeyTap`/`EncodeKeySeq`/`EncodeKeyHold`/`EncodeKeyRelease`/`EncodeKeyType`（darwin 命令直通车按键模拟 push 帧 + `KeyComboData` 入参）；`CalcKeyHash`/`ParseKeyHash` 热键哈希 |
| `server.go` | JSON Named Pipe 服务端（`\\.\pipe\tsf_ime_service`）— 遗留，当前未使用 |

## For AI Agents

### Working In This Directory
- **当前实际使用**的是 `binary_codec.go` 和 `binary_protocol.go`，由 `bridge` 包调用
- 热键哈希函数为 `CalcKeyHash(modifiers, keyCode uint32) uint32`；`ParseKeyHash(hash uint32)` 为逆向解码
- `EncodeStatusUpdateEx` 与 `EncodeStatusUpdate` 的区别：前者多一个 `hostRenderAvail bool` 参数，会设置 `StatusHostRenderAvail` 标志位
- `EncodeActivationStatusPush`（命令码 `CmdActivationStatusPush=0x020C`）与 `EncodeStatusUpdateEx` 的载荷格式相同（含 hotkeys + hostRenderAvail + iconLabel），区别仅在 command 字段；用于 IMEActivated/FocusGained 异步化后通过 push pipe 推送状态回包，C++ 端 AsyncReader 解析后 Post 到 TSF 线程做 `_SyncStateFromResponse` + `_EnsureHostRenderSetup`
- 与 `EncodeStatePush`（`CmdStatePush=0x0206`）的区别：StatePush 是状态变更广播（hotkey 不变所以不带），ActivationStatusPush 是 activation 握手回包（必须带完整 hotkeys + hostRenderAvail）
- `CmdHostRenderSetup`（下行 0x0501）和 `CmdHostRenderRequest`（上行 0x0501，C++ DLL 请求）共用同一命令码值，但方向不同
- `CmdHostRenderFrame`（下行 0x0502, push）darwin 专用：Win 用命名 Event 通知 host render 新帧, darwin 无等价 API 改走 push 通道发 `HostRenderFramePayload`(seq+几何), 客户端据 seq 从 SHM 取帧 blit
- `CmdCandidateRects`（下行 0x0503, push）darwin 专用：候选命中矩形 `[]CandidateHitRect` (panel-local), `EncodeCandidateRects`; 供 IMKit `.app` NSPanel 鼠标 hit-test
- `CmdCandidateSelect`（上行 0x020D）darwin 专用：NSPanel 鼠标点中候选, payload=pageLocalIndex u32; Go 选词结果走 push 通道 (`PushCommitTextToActiveClient`) 异步交付
- `CmdCandidateHover`（上行 0x020E）darwin 专用：NSPanel 鼠标悬停候选, payload=pageLocalIndex i32 (-1=无); server_darwin 双派发——forwarder 回调缓存候选按 hoverIndex 重渲染高亮, 同时 `Coordinator.HandleCandidateHover` 触发 tooltip 异步查询
- `CmdCandidateRects` 中 index<0 为翻页按钮 (-1=上页 -2=下页), 客户端点中合成 PgUp/PgDn 键
- `CmdModeStatus`（下行 0x0504, push）darwin 专用：输入模式状态指示器, `EncodeModeStatus(flags, effectiveMode, label)`; payload=flags(u32)+effectiveMode(u32)+labelLen(u32)+label(UTF-8); flags 复用 `StatusChineseMode/StatusFullWidth/StatusChinesePunct/StatusCapsLock/StatusToolbarVisible` 位; forwarder 收 `CmdToolbarShow/Update/Hide` 转译为此帧, .app 据此更新菜单栏 NSStatusItem
- `CmdCandidateMenuFlags`（下行 0x0505, push）darwin 专用：当前页候选右键菜单禁用位, `EncodeCandidateMenuFlags(flags []byte)`; 每候选 1 字节, 位 `MenuFlagDisableTop/Move/Delete/Reset/Copy`(0x01..0x10); .app 据此 disable 候选右键 NSMenuItem
- `CmdMenuShow`（下行 0x0506）darwin 专用：统一菜单树, 是上行 `CmdShowContextMenu`(0x020A) 的请求-响应回包 (经 request 连接 conn.Write, 非 push); payload 见 `bridge.encodeUnifiedMenuPayload` (count u32 + 递归 item: id i32 + flags u8[0x01 sep/0x02 checked/0x04 disabled] + labelLen u32 + label + childCount u32 + children)
- `CmdOpenSettings`（下行 0x0507, push）darwin 专用：请求 .app 打开设置应用, `EncodeOpenSettings(page string)`; payload=page(UTF-8); forwarder 收 `uicmd.CmdSettingsOpen` 转译为此帧, .app 经 NSWorkspace 带 `--page=` 启动/激活设置 app
- `CmdTooltipShow`（下行 0x0508, push）darwin 专用：候选悬停 tooltip, `EncodeTooltipShow(text, bgColor, fgColor, fontPath string)`; payload 四段长度前缀字符串 text+bg+fg+fontPath; bg/fg 为 `#RRGGBB[AA]` 主题色, fontPath 为拆字字根字体文件绝对路径 (空=无需特殊字体, .app 注册后级联回退渲染 PUA 字根); 位置由 .app 据悬停候选屏幕矩形自定
- `CmdTooltipHide`（下行 0x0509, push）darwin 专用：隐藏 tooltip, `EncodeTooltipHide()` 空 payload
- `CmdStatusShow`（下行 0x050A, push）darwin 专用：模式切换状态气泡, `EncodeStatusShow(text, bgColor, fgColor string, x, y, durationMs int32)`; payload 三段长度前缀字符串 text+bg+fg + x(i32)+y(i32)+durationMs(i32); text 为合并短文 (如 "中 ，"), bg/fg 为 #RRGGBB[AA] (主题 ModeIndicator 配色叠加 opacity), x/y 为 caret 底部下方锚点 (与候选窗口同位置), durationMs>0 到点自动隐藏 (temp 模式) / ==0 常驻 (always); forwarder 收 `uicmd.CmdStatusShow` 据 config 合成
- `CmdStatusHide`（下行 0x050B, push）darwin 专用：隐藏状态气泡, `EncodeStatusHide()` 空 payload
- `CmdToastShow`（下行 0x050C, push）darwin 专用：Toast 通知 (词库就绪/错误等屏幕级提示), `EncodeToastShow(title, message, bgColor, fgColor, accentColor, position string, durationMs, maxWidth int32)`; payload 六段长度前缀字符串 (title+message+bg+fg+accent+position) + durationMs(i32)+maxWidth(i32); bg/fg 取主题 Tooltip 配色 (强制不透明), accent 按级别取 `ui.ToastAccentColor`, position 为 "bottom_right"/"center" (.app 据此在工作区落位); durationMs 0=默认5000 / >0自动隐藏 / <0常驻; forwarder 收 `uicmd.CmdToastShow` 合成
- `CmdToastHide`（下行 0x050D, push）darwin 专用：隐藏 Toast, `EncodeToastHide()` 空 payload
- `CmdKeyTap`/`CmdKeySeq`/`CmdKeyHold`/`CmdKeyRelease`（下行 0x050E/0x050F/0x0510/0x0511, push）darwin 专用：命令直通车按键模拟, `EncodeKeyTap`/`EncodeKeySeq`/`EncodeKeyHold`/`EncodeKeyRelease`; 单 combo wire = key(u32 len + UTF-8) + modCount(u32) + modCount×(u32 len + UTF-8), KeySeq = comboCount(u32) + N×combo; 编码入参 `KeyComboData{Key, Modifiers}`; forwarder 收 `uicmd.CmdKeyTap/Seq/Hold/Release` 转译, IMKit `.app` 用 CGEvent 向聚焦应用合成 (key.tap/seq/hold/release, 需「辅助功能」授权)
- `CmdKeyType`（下行 0x0512, push）darwin 专用：命令直通车 key.type / clip.paste 文本上屏, `EncodeKeyType(text)`; payload = 整段 UTF-8 (无长度前缀, 同 `EncodeOpenSettings` 风格); forwarder 收 `uicmd.CmdKeyType` 转译, .app 经 `client.insertText` 上屏 (不模拟按键, 免辅助功能授权)
- `CmdCandidateContextMenu`（上行 0x020F）darwin 专用：候选右键菜单动作, payload=index i32 + actionLen u32 + action(UTF-8); Coordinator.HandleCandidateContextMenu 按 action 派发 move/delete/reset/copy
- `CmdMenuAction`（上行 0x0210）darwin 专用：统一菜单项被选中, payload=id i32; Coordinator.HandleUnifiedMenuAction 按 id 派发
- `SharedRenderHeader` 固定 64 字节：前 40 字节有效字段，后 24 字节保留；后跟 BGRA 像素数据
- `CmdBatchEvents` 是批量事件命令，`bridge` 对其有特殊处理路径
- `IsAsyncRequest(header)` 判断是否为不需要响应的异步请求（版本字段高位为 `AsyncFlag=0x8000`）
- 修改命令码时需同步修改 C++ TSF Bridge 侧的枚举定义

### Testing Requirements
- 编解码往返测试可作为单元测试添加
- 与 C++ 侧协议兼容性需集成测试

### Common Patterns
- 消息格式：`[Header 8B][Payload]`，Header 包含协议版本、命令码和 Payload 长度
- `bridge` 包直接使用 `ipc.NewBinaryCodec()` 实例，不需要直接与 `ipc.Server` 交互

## Dependencies
### Internal
- 无（被 `bridge`、`hotkey`、`coordinator` 引用）

### External
- `golang.org/x/sys/windows` — Named Pipe API（server.go 遗留）

<!-- MANUAL: -->
