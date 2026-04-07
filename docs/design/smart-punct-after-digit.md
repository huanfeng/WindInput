# 数字后智能标点转换

## 功能概述

在中文输入模式下，输入数字后紧接输入的句号（。）和逗号（，）自动转换为英文标点（`.` 和 `,`），方便输入 IP 地址（192.168.1.1）、小数（3.14）、千分位（1,000）等场景。

- 配置项：`input.smart_punct_after_digit`（默认启用）
- 仅影响句号和逗号两个标点
- 英文模式下本身就是英文标点，不需要处理

## 判断逻辑

核心问题：**如何判断「光标前是数字」？**

不同应用对 TSF 的支持程度不同，单一方案无法覆盖所有场景，因此采用三层检测机制，按优先级逐层回退：

```
prevChar 来源优先级：

1. ITfTextEditSink::OnEndEdit 缓存（_cachedPrevChar）
   ↓ 为 0 时回退
2. OnTestKeyDown 直通数字追踪（_lastPassthroughDigit）
   ↓ 为 0 时回退
3. Go 端 lastOutputWasDigit 状态追踪
```

### 第一层：TSF OnEndEdit 缓存（主路径）

**原理**：实现 `ITfTextEditSink` 接口，在每次文本编辑完成后通过 `GetSelection` + `ShiftStart(-1)` + `GetText` 获取光标前一个字符并缓存。

**覆盖场景**：记事本、浏览器等标准 TSF 应用。

**代码路径**：
- C++：`TextService::OnEndEdit` → `_cachedPrevChar`
- C++：`KeyEventSink::_SendKeyToService` → 读取 `_cachedPrevChar` 作为 `prevChar`
- 协议：`KeyPayload.prevChar`（uint16，第 16-17 字节）
- Go：`KeyEventData.PrevChar` → `shouldSmartPunct` 检查 `prevChar >= '0' && prevChar <= '9'`

### 第二层：C++ 直通数字追踪（回退路径）

**原理**：某些编辑器（如 EverEdit）的 TSF 支持不完整，`OnEndEdit` 无法正确获取文本。此时 C++ 端在 `OnTestKeyDown` 中追踪直通的数字键。

**背景**：在中文模式无输入会话（无候选词）时，数字键被分类为 `HotkeyType::Number`，`pfEaten=FALSE` 直接传给应用，不经过 Go 处理。这是为了兼容 WindTerm 等应用（`OnTestKeyDown(TRUE) + OnKeyDown(FALSE)` 翻转行为会导致按键被吞）。

**状态管理**：
| 事件 | `_lastPassthroughDigit` |
|------|-------------------------|
| 数字键直通（pfEaten=FALSE） | 设为该数字字符，同时记录 `_digitCaretY` |
| 非数字键直通 | 清除为 0 |
| `_SendKeyToService` 中使用 | 消费后清除为 0（防止 `123。。。。` 后续标点误转） |
| 光标 Y 坐标变化 | 清除为 0（检测跨行鼠标移动） |
| `OnEndEdit` 选区变化（非组合态） | 通过 `ClearPassthroughDigit()` 清除 |
| `ResetComposingState`（焦点变化） | 清除为 0 |

**代码路径**：
- C++：`KeyEventSink::OnTestKeyDown` → 记录 `_lastPassthroughDigit`
- C++：`KeyEventSink::_SendKeyToService` → 若 `_cachedPrevChar == 0`，使用 `_lastPassthroughDigit` 作为 `prevChar`

### 第三层：Go 端状态追踪（兜底路径）

**原理**：当 C++ 两层都不可用（`prevChar=0`）时，Go 端通过 `lastOutputWasDigit` 标志追踪。仅在数字键到达 Go 时有效（即有输入会话的数字选择后的直通场景）。

**状态管理**：采用 save-and-reset 模式——`HandleKeyEvent` 入口处保存并重置，仅数字直通路径重新设为 true。

| 事件 | `lastOutputWasDigit` |
|------|---------------------|
| `HandleKeyEvent` 入口 | 保存为 `prevDigitState`，重置为 false |
| 数字键直通（handleNumberKey 返回 nil） | 设为 true |
| 小键盘数字输出 | 设为 true |
| `HandleFocusLost` / `HandleFocusGained` | 重置为 false |
| `HandleCompositionTerminated` | 重置为 false |
| `HandleIMEDeactivated` | 重置为 false |
| `HandleSelectionChanged` | 重置为 false |

**代码路径**：
- Go：`handle_key_event.go` → `prevDigitState`
- Go：`handle_punctuation.go` → `shouldSmartPunct` 的 `afterDigit` 参数

## IPC 协议变更

### KeyPayload 扩展（16 → 18 字节）

```
偏移  大小  字段
0     4     keyCode
4     4     scanCode
8     4     modifiers
12    1     eventType
13    1     toggles
14    2     eventSeq
16    2     prevChar    ← 新增：光标前字符（0 = 不可用）
```

Go 端向后兼容：`len(buf) >= 18` 时才读取 `prevChar`，否则默认为 0。

### 新增命令 CMD_SELECTION_CHANGED (0x0302)

**方向**：C++ → Go（异步，无需响应）

**触发条件**：`OnEndEdit` 检测到选区变化且无活跃组合。

**载荷**（4 字节）：
```
偏移  大小  字段
0     2     prevChar    光标前字符
2     2     reserved    保留
```

**Go 端处理**：`HandleSelectionChanged` → 重置 `lastOutputWasDigit`。

## 已知局限

1. **同行鼠标移动**：在不支持 `OnEndEdit` 的编辑器（如 EverEdit）中，同一行内的鼠标点击无法被检测，可能导致误转换。跨行鼠标移动可通过光标 Y 坐标对比检测。
2. **数字键不经过 Go**：无输入会话时数字键直通，Go 端无法感知。依赖 C++ 端追踪或 TSF prevChar。
3. **应用兼容性差异**：取决于应用的 TSF 实现质量。标准应用（记事本、浏览器）通过第一层完整支持；TSF 支持不完整的编辑器通过第二层部分支持。

## 文件索引

| 模块 | 文件 | 关键内容 |
|------|------|----------|
| C++ 协议 | `wind_tsf/include/BinaryProtocol.h` | KeyPayload.prevChar, CMD_SELECTION_CHANGED, SelectionChangedPayload |
| C++ IPC | `wind_tsf/include/IPCClient.h` | SendKeyEvent(+prevChar), SendSelectionChanged |
| C++ IPC | `wind_tsf/src/IPCClient.cpp` | 实现 |
| C++ TSF | `wind_tsf/include/TextService.h` | ITfTextEditSink, _cachedPrevChar, GetLastKnownCaretY |
| C++ TSF | `wind_tsf/src/TextService.cpp` | OnEndEdit, _AdviseTextEditSink, _UnadviseTextEditSink |
| C++ 按键 | `wind_tsf/include/KeyEventSink.h` | _lastPassthroughDigit, _digitCaretY, ClearPassthroughDigit |
| C++ 按键 | `wind_tsf/src/KeyEventSink.cpp` | OnTestKeyDown 追踪, _SendKeyToService prevChar 合成 |
| Go 协议 | `wind_input/internal/ipc/binary_protocol.go` | KeyPayload.PrevChar, CmdSelectionChanged |
| Go 编解码 | `wind_input/internal/ipc/binary_codec.go` | DecodeKeyPayload 兼容 18 字节 |
| Go 桥接 | `wind_input/internal/bridge/protocol.go` | KeyEventData.PrevChar, HandleSelectionChanged |
| Go 桥接 | `wind_input/internal/bridge/server_handler.go` | handleSelectionChanged |
| Go 协调 | `wind_input/internal/coordinator/coordinator.go` | lastOutputWasDigit |
| Go 协调 | `wind_input/internal/coordinator/handle_key_event.go` | save-and-reset, 数字追踪 |
| Go 协调 | `wind_input/internal/coordinator/handle_punctuation.go` | shouldSmartPunct 三层判断 |
| Go 协调 | `wind_input/internal/coordinator/handle_lifecycle.go` | HandleSelectionChanged, 生命周期重置 |
| 配置 | `wind_input/pkg/config/config.go` | SmartPunctAfterDigit |
| 前端 | `wind_setting/frontend/src/pages/InputPage.vue` | 设置界面开关 |
| 前端 | `wind_setting/frontend/src/api/settings.ts` | smart_punct_after_digit 字段 |
