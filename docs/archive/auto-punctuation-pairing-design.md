# 自动标点配对功能设计文档

> 日期：2026-04-08
> 状态：已确认

## 概述

为 WindInput 输入法实现自动标点配对功能。当用户在中文模式下输入左括号类标点时，自动补全对应的右标点并将光标置于中间，提升打字效率。

## 功能范围

### 首期实现

1. **自动配对** — 输入左标点，自动上屏左右标点对，光标定位在中间
2. **智能跳过** — 光标右侧已有匹配右标点时，输入右标点只跳过不重复插入
3. **智能删除** — Backspace 时，若光标处于配对中间，连带删除右标点
4. **全局开关** — 可在配置中开启/关闭
5. **可配置配对项** — 用户可自定义哪些标点参与配对
6. **应用黑名单** — 指定进程中禁用配对功能

### 生效条件

- 仅在 **中文模式 + 中文标点模式** 下生效
- 英文模式或英文标点模式下不配对

### 明确排除

- 英文标点配对（避免与编辑器/IDE 自带功能冲突）
- 选中文本包裹（后续迭代）
- 引号配对改造（保持现有左右交替逻辑不变）
- Composition 事务包装（单次 EditSession 完成）
- 预置终端黑名单（实测后再定）

## 配置结构

在 `config.yaml` 中新增 `auto_pair` 配置：

```yaml
input:
  auto_pair:
    enabled: true                    # 全局开关
    pairs:                           # 可配置的配对标点（中文标点）
      - ["（", "）"]
      - ["【", "】"]
      - ["｛", "｝"]
      - ["《", "》"]
      - ["〈", "〉"]
    blacklist:                       # 应用黑名单（进程名，不区分大小写）
      []
```

- `enabled: false` 时整个功能关闭
- `pairs` 数组允许用户自由增删配对项
- `blacklist` 初始为空，用户按需添加进程名（如 `Code.exe`）

## 架构设计

### 总体方案：逻辑全在 Go 层

与当前架构一致——Go 层决策，C++ 层执行。

- Go 层：判断是否配对、维护配对栈、决定响应类型
- C++ 层：执行文本插入和光标操作（通过 TSF API）
- IPC 协议：扩展响应类型和字段

### Go 层数据结构

#### PairTracker（配对栈）

新建文件 `wind_input/internal/transform/pair_tracker.go`：

```go
type PairEntry struct {
    Left  rune  // 左标点
    Right rune  // 对应的右标点
}

type PairTracker struct {
    stack    []PairEntry   // 最近插入的配对记录（LIFO）
    pairMap  map[rune]rune // 左→右映射（从配置加载）
    rightSet map[rune]bool // 右标点集合（用于快速判断）
}
```

方法：

- `Push(left, right rune)` — 记录一次配对插入
- `Peek() (PairEntry, bool)` — 查看栈顶
- `Pop() (PairEntry, bool)` — 弹出栈顶
- `Clear()` — 清空栈
- `IsLeft(r rune) bool` — 判断是否为左标点
- `IsRight(r rune) bool` — 判断是否为右标点
- `GetRight(left rune) (rune, bool)` — 获取左标点对应的右标点
- `UpdatePairs(pairs [][]string)` — 从配置更新映射表

#### Coordinator 集成

在 `Coordinator` 中新增字段：

```go
type Coordinator struct {
    // ...existing fields...
    pairTracker  *transform.PairTracker  // 配对栈
    autoPairCfg  *config.AutoPairConfig  // 配对配置
}
```

### 配对栈失效时机

栈在以下事件发生时清空：

| 事件 | 原因 |
|------|------|
| 提交候选词 | 光标位置已变 |
| 输入普通字符（非配对标点） | 光标和配对之间插入了内容 |
| 按方向键 | 光标位置已变 |
| 切换中英文模式 | 上下文变化 |
| 切换标点模式 | 上下文变化 |
| 切换焦点/应用 | 上下文完全变化 |
| 按 Enter/Escape | 上下文变化 |
| 配置热更新 | 配对规则可能已变 |

连续输入多个左括号时，栈中会有多个条目，智能跳过和删除按 LIFO 顺序处理。

## 核心逻辑流程

### 自动配对

```
用户输入字符 r
    ↓
是否中文模式 + 中文标点模式？ → 否 → 正常流程
    ↓ 是
auto_pair.enabled == true？ → 否 → 正常流程
    ↓ 是
r 经过中英文标点转换后得到 converted
    ↓
converted 是否在 pairMap 的左标点中？ → 否 → 正常流程
    ↓ 是
当前进程是否在黑名单中？ → 是 → 正常流程（只上屏左标点）
    ↓ 否
上屏 "converted + 右标点"，CursorOffset = 1
Push(left, right) 到配对栈
```

### 智能跳过

```
用户输入字符 r
    ↓
r 经过转换后得到 converted
    ↓
配对栈是否非空？ → 否 → 正常流程
    ↓ 是
栈顶的 Right == converted？ → 否 → 清空栈，正常流程
    ↓ 是
Pop 栈顶
返回 ResponseType = MoveCursorRight（光标右移 1 格跳过右标点）
```

### 智能删除

```
用户按下 Backspace
    ↓
当前是否有输入缓冲区（inputBuffer 非空）？ → 是 → 现有退格逻辑
    ↓ 否
配对栈是否非空？ → 否 → PassThrough（让应用处理）
    ↓ 是
Pop 栈顶
返回 ResponseType = DeletePair（删除光标左右各一个字符）
```

## IPC 协议变更

### 请求方向（C++ → Go）

`KeyEventData` 新增字段：

```go
type KeyEventData struct {
    // ...existing fields...
    ProcessName string  // 当前焦点窗口进程名
}
```

C++ 层通过 `GetForegroundWindow()` → `GetWindowThreadProcessId()` → `OpenProcess()` + `QueryFullProcessImageName()` 获取进程名。

### 响应方向（Go → C++）

新增三种响应类型：

| 响应类型 | 说明 |
|---------|------|
| `InsertTextWithCursor` | 插入文本，光标定位到 `len(text) - CursorOffset` 处 |
| `MoveCursorRight` | 光标右移 1 个字符（智能跳过） |
| `DeletePair` | 删除光标左侧 1 字符 + 右侧 1 字符（智能删除） |

`KeyEventResult` 新增字段：

```go
type KeyEventResult struct {
    // ...existing fields...
    CursorOffset int  // InsertTextWithCursor 时使用，从文本末尾向左的字符数
}
```

## C++ TSF 层实现

### 新增 EditSession 类型

#### CInsertTextWithCursorEditSession

```
1. 获取当前选区 ITfRange
2. 调用 range->SetText() 插入配对文本（如 "（）"）
3. 将 range 起始点设置到插入文本末尾
4. 将 range 向左偏移 CursorOffset 个字符
5. 调用 ITfContext::SetSelection() 设置光标
```

#### CMoveCursorEditSession

```
1. 获取当前选区 ITfRange
2. 将 range 向右偏移 1 个字符（跳过右标点）
3. range 折叠为 0 长度
4. 调用 SetSelection() 设置光标
```

#### CDeletePairEditSession

```
1. 获取当前选区 ITfRange
2. 克隆 range，向左扩展 1 字符（覆盖左标点）
3. 克隆 range，向右扩展 1 字符（覆盖右标点）
4. 合并两个 range，调用 SetText("") 删除
5. 调用 SetSelection() 设置光标
```

### 进程名获取

在 `KeyEventSink::OnKeyDown` 中获取前台窗口进程名并通过 IPC 传递给 Go 层：

```cpp
HWND hwnd = GetForegroundWindow();
DWORD pid;
GetWindowThreadProcessId(hwnd, &pid);
// OpenProcess + QueryFullProcessImageName 获取进程名
// 提取文件名部分，通过 KeyEventData.ProcessName 传递
```

## 边界情况处理

| 场景 | 处理方式 |
|------|---------|
| 输入缓冲区非空时按左括号键 | `punct_commit` 启用：先提交候选词，再执行配对；禁用：不拦截，PassThrough |
| 连续输入多个左括号 | 栈中多条记录，LIFO 顺序跳过/删除 |
| 配对后输入普通文字再按右括号 | 栈已清空，正常上屏右括号 |
| 全角模式下 | 配对功能正常工作（配对的是中文标点，与全角无关） |
| 配对后立即切换应用 | C++ 层检测到焦点变化，通知 Go 层清栈 |
| 配置热更新 | 修改 `auto_pair` 后实时生效，清空当前栈 |

## 文件变更清单

### Go 层

| 文件 | 变更 |
|------|------|
| `wind_input/pkg/config/config.go` | 新增 `AutoPairConfig` 结构体 |
| `wind_input/internal/transform/pair_tracker.go` | **新建**，PairTracker 实现 |
| `wind_input/internal/coordinator/coordinator.go` | 新增 `pairTracker` 字段，清空时机挂接 |
| `wind_input/internal/coordinator/handle_punctuation.go` | 集成配对判断和智能跳过逻辑 |
| `wind_input/internal/coordinator/handle_key_event.go` | 在适当位置清空配对栈 |
| `wind_input/internal/coordinator/handle_key_action.go` | `handleBackspace` 中集成智能删除 |
| `wind_input/internal/coordinator/reload_handler.go` | 支持热更新 `auto_pair` 配置 |
| `wind_input/internal/bridge/protocol.go` | 新增响应类型和字段 |
| `wind_input/internal/bridge/codec.go` | 编解码新字段 |
| `build/data/config.yaml` | 新增默认 `auto_pair` 配置 |

### C++ 层

| 文件 | 变更 |
|------|------|
| `wind_tsf/include/BinaryProtocol.h` | 新增响应类型常量和字段 |
| `wind_tsf/src/IPCClient.cpp` | 解析新响应类型和字段 |
| `wind_tsf/src/TextService.cpp` | 分发三种新响应类型 |
| `wind_tsf/src/CaretEditSession.cpp` | 新增三个 EditSession 类 |
| `wind_tsf/include/CaretEditSession.h` | 对应头文件声明 |
| `wind_tsf/src/KeyEventSink.cpp` | 获取进程名并通过 IPC 传递 |
