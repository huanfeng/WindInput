# 自动标点配对 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在中文模式下输入左括号类标点时自动补全右括号并将光标置于中间，支持智能跳过和智能删除。

**Architecture:** Go 层负责所有配对决策逻辑（PairTracker 配对栈），通过扩展 IPC 协议向 C++ TSF 层发送三种新操作指令（插入文本并定位光标、光标右移、删除配对）。C++ 层新增对应的 EditSession 类执行 TSF API 操作。进程黑名单复用 Coordinator 已有的 `activeProcessName` 字段，无需修改上行协议。

**Tech Stack:** Go (coordinator/transform/config/bridge), C++ (TSF EditSession/BinaryProtocol/IPCClient/KeyEventSink)

**Spec:** `docs/design/2026-04-08-auto-punctuation-pairing-design.md`

---

## File Structure

| File | Responsibility |
|------|---------------|
| `wind_input/pkg/config/config.go` | `AutoPairConfig` 结构体，默认值 |
| `wind_input/internal/transform/pair_tracker.go` | **新建** — PairTracker 配对栈 |
| `wind_input/internal/transform/pair_tracker_test.go` | **新建** — PairTracker 单元测试 |
| `wind_input/internal/bridge/protocol.go` | 新增 3 个 ResponseType 常量和 CursorOffset 字段 |
| `wind_input/internal/ipc/binary_codec.go` | 新增 3 个编码方法 |
| `wind_input/internal/ipc/constants.go`（或同文件） | 新增 3 个 CMD 常量 |
| `wind_input/internal/bridge/server_handler.go` | handleKeyEvent 分发新响应类型 |
| `wind_input/internal/coordinator/coordinator.go` | 新增 `pairTracker` 字段 |
| `wind_input/internal/coordinator/handle_punctuation.go` | 配对和智能跳过逻辑 |
| `wind_input/internal/coordinator/handle_key_action.go` | handleBackspace 智能删除 |
| `wind_input/internal/coordinator/handle_key_event.go` | 配对栈清空时机 |
| `wind_input/internal/coordinator/handle_lifecycle.go` | 焦点丢失时清空栈 |
| `wind_input/internal/coordinator/handle_config.go` | UpdateInputConfig 支持 auto_pair 热更新 |
| `wind_input/internal/coordinator/reload_handler.go` | 无需改动（已通过 UpdateInputConfig 链路覆盖） |
| `build/data/config.yaml` | 新增默认 auto_pair 配置 |
| `wind_tsf/include/BinaryProtocol.h` | 新增 3 个 CMD 常量和 ResponseType 枚举值 |
| `wind_tsf/src/IPCClient.cpp` | _ParseResponse 解析 3 种新命令 |
| `wind_tsf/src/TextService.cpp` | 新增 3 个 EditSession 类 |
| `wind_tsf/src/TextService.h` | 新增 3 个公共方法声明 |
| `wind_tsf/src/KeyEventSink.cpp` | _HandleServiceResponse 分发 3 种新响应 |

---

### Task 1: 新增 AutoPairConfig 配置结构

**Files:**
- Modify: `wind_input/pkg/config/config.go`
- Modify: `build/data/config.yaml`

- [ ] **Step 1: 在 config.go 中新增 AutoPairConfig 结构体**

在 `TempPinyinConfig` 结构体之后添加：

```go
// AutoPairConfig 自动标点配对配置
type AutoPairConfig struct {
	Enabled   bool       `yaml:"enabled" json:"enabled"`
	Pairs     [][]string `yaml:"pairs" json:"pairs"`
	Blacklist []string   `yaml:"blacklist" json:"blacklist"`
}
```

在 `InputConfig` 结构体末尾添加字段：

```go
AutoPair AutoPairConfig `yaml:"auto_pair" json:"auto_pair"`
```

- [ ] **Step 2: 在 DefaultConfig 中设置默认值**

在 `DefaultConfig()` 函数的 `Input` 字段中添加：

```go
AutoPair: config.AutoPairConfig{
	Enabled: true,
	Pairs: [][]string{
		{"（", "）"},
		{"【", "】"},
		{"｛", "｝"},
		{"《", "》"},
		{"〈", "〉"},
	},
	Blacklist: []string{},
},
```

- [ ] **Step 3: 在 config.yaml 中添加默认配置**

在 `build/data/config.yaml` 的 `input:` 节的末尾（`temp_pinyin` 之后）添加：

```yaml
  auto_pair:
    enabled: true
    pairs:
      - ["（", "）"]
      - ["【", "】"]
      - ["｛", "｝"]
      - ["《", "》"]
      - ["〈", "〉"]
    blacklist: []
```

- [ ] **Step 4: 运行 go fmt 并确认编译通过**

Run: `cd D:/Develop/workspace/go_dev/WindInput/wind_input && go fmt ./pkg/config/ && go build ./...`
Expected: 无错误

---

### Task 2: 实现 PairTracker 配对栈

**Files:**
- Create: `wind_input/internal/transform/pair_tracker.go`
- Create: `wind_input/internal/transform/pair_tracker_test.go`

- [ ] **Step 1: 编写 PairTracker 测试**

```go
package transform

import "testing"

func TestPairTracker_BasicPairing(t *testing.T) {
	pt := NewPairTracker([][]string{{"（", "）"}, {"【", "】"}})

	// 左标点识别
	if !pt.IsLeft('（') {
		t.Error("（ should be left")
	}
	if pt.IsLeft('）') {
		t.Error("） should not be left")
	}

	// 获取右标点
	right, ok := pt.GetRight('（')
	if !ok || right != '）' {
		t.Errorf("GetRight('（') = %c, %v; want ）, true", right, ok)
	}

	// 右标点识别
	if !pt.IsRight('）') {
		t.Error("） should be right")
	}
	if pt.IsRight('（') {
		t.Error("（ should not be right")
	}
}

func TestPairTracker_StackOperations(t *testing.T) {
	pt := NewPairTracker([][]string{{"（", "）"}, {"【", "】"}})

	// 空栈
	_, ok := pt.Peek()
	if ok {
		t.Error("Peek on empty stack should return false")
	}

	// Push + Peek
	pt.Push('（', '）')
	entry, ok := pt.Peek()
	if !ok || entry.Right != '）' {
		t.Error("Peek should return ）")
	}

	// 连续 Push（嵌套括号）
	pt.Push('【', '】')
	entry, ok = pt.Peek()
	if !ok || entry.Right != '】' {
		t.Error("Peek should return 】 (LIFO)")
	}

	// Pop
	entry, ok = pt.Pop()
	if !ok || entry.Right != '】' {
		t.Error("Pop should return 】")
	}
	entry, ok = pt.Pop()
	if !ok || entry.Right != '）' {
		t.Error("Pop should return ）")
	}
	_, ok = pt.Pop()
	if ok {
		t.Error("Pop on empty stack should return false")
	}
}

func TestPairTracker_Clear(t *testing.T) {
	pt := NewPairTracker([][]string{{"（", "）"}})
	pt.Push('（', '）')
	pt.Push('（', '）')
	pt.Clear()
	_, ok := pt.Peek()
	if ok {
		t.Error("Peek after Clear should return false")
	}
}

func TestPairTracker_UpdatePairs(t *testing.T) {
	pt := NewPairTracker([][]string{{"（", "）"}})
	if !pt.IsLeft('（') {
		t.Error("（ should be left before update")
	}

	// 更新配对：移除（），添加《》
	pt.UpdatePairs([][]string{{"《", "》"}})
	if pt.IsLeft('（') {
		t.Error("（ should not be left after update")
	}
	if !pt.IsLeft('《') {
		t.Error("《 should be left after update")
	}

	// 更新后栈应被清空
	pt.Push('《', '》')
	pt.UpdatePairs([][]string{{"《", "》"}})
	_, ok := pt.Peek()
	if ok {
		t.Error("Stack should be cleared after UpdatePairs")
	}
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./internal/transform/ -run TestPairTracker -v`
Expected: 编译失败，NewPairTracker 未定义

- [ ] **Step 3: 实现 PairTracker**

```go
package transform

// PairEntry 记录一次配对插入
type PairEntry struct {
	Left  rune
	Right rune
}

// PairTracker 追踪自动配对状态，用于智能跳过和智能删除
type PairTracker struct {
	stack    []PairEntry
	pairMap  map[rune]rune // 左→右
	rightSet map[rune]bool // 右标点集合
}

// NewPairTracker 创建配对追踪器，pairs 格式为 [["（","）"], ...]
func NewPairTracker(pairs [][]string) *PairTracker {
	pt := &PairTracker{}
	pt.buildMaps(pairs)
	return pt
}

func (pt *PairTracker) buildMaps(pairs [][]string) {
	pt.pairMap = make(map[rune]rune, len(pairs))
	pt.rightSet = make(map[rune]bool, len(pairs))
	for _, p := range pairs {
		if len(p) != 2 {
			continue
		}
		left := []rune(p[0])
		right := []rune(p[1])
		if len(left) != 1 || len(right) != 1 {
			continue
		}
		pt.pairMap[left[0]] = right[0]
		pt.rightSet[right[0]] = true
	}
}

// UpdatePairs 更新配对映射（配置热更新时调用），同时清空栈
func (pt *PairTracker) UpdatePairs(pairs [][]string) {
	pt.buildMaps(pairs)
	pt.stack = nil
}

// IsLeft 判断是否为左标点
func (pt *PairTracker) IsLeft(r rune) bool {
	_, ok := pt.pairMap[r]
	return ok
}

// IsRight 判断是否为右标点
func (pt *PairTracker) IsRight(r rune) bool {
	return pt.rightSet[r]
}

// GetRight 获取左标点对应的右标点
func (pt *PairTracker) GetRight(left rune) (rune, bool) {
	r, ok := pt.pairMap[left]
	return r, ok
}

// Push 记录一次配对插入
func (pt *PairTracker) Push(left, right rune) {
	pt.stack = append(pt.stack, PairEntry{Left: left, Right: right})
}

// Peek 查看栈顶
func (pt *PairTracker) Peek() (PairEntry, bool) {
	if len(pt.stack) == 0 {
		return PairEntry{}, false
	}
	return pt.stack[len(pt.stack)-1], true
}

// Pop 弹出栈顶
func (pt *PairTracker) Pop() (PairEntry, bool) {
	if len(pt.stack) == 0 {
		return PairEntry{}, false
	}
	entry := pt.stack[len(pt.stack)-1]
	pt.stack = pt.stack[:len(pt.stack)-1]
	return entry, true
}

// Clear 清空栈
func (pt *PairTracker) Clear() {
	pt.stack = nil
}

// IsEmpty 判断栈是否为空
func (pt *PairTracker) IsEmpty() bool {
	return len(pt.stack) == 0
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./internal/transform/ -run TestPairTracker -v`
Expected: 全部 PASS

- [ ] **Step 5: 格式化代码**

Run: `cd D:/Develop/workspace/go_dev/WindInput/wind_input && go fmt ./internal/transform/`

---

### Task 3: 扩展 IPC 协议（Go 侧）

**Files:**
- Modify: `wind_input/internal/bridge/protocol.go`
- Modify: `wind_input/internal/ipc/binary_codec.go`
- Modify: `wind_input/internal/bridge/server_handler.go`

需要先找到 ipc 常量定义的位置。

- [ ] **Step 1: 在 bridge/protocol.go 中新增 ResponseType 和字段**

在 `ResponseTypeConsumed` 之后添加三个新类型：

```go
ResponseTypeInsertTextWithCursor ResponseType = "insert_text_with_cursor" // 插入文本并定位光标
ResponseTypeMoveCursorRight      ResponseType = "move_cursor_right"       // 光标右移（智能跳过）
ResponseTypeDeletePair           ResponseType = "delete_pair"             // 删除配对（智能删除）
```

在 `KeyEventResult` 结构体中添加字段：

```go
CursorOffset int // For InsertTextWithCursor: 光标从文本末尾向左偏移的字符数
```

- [ ] **Step 2: 在 ipc 包中新增 CMD 常量**

在 `wind_input/internal/ipc/binary_protocol.go` 中找到 `CmdConsumed` 常量定义（约第40行），在其后添加：

```go
CmdCommitTextWithCursor uint16 = 0x0106 // Commit text with cursor offset
CmdMoveCursor           uint16 = 0x0107 // Move cursor (skip over)
CmdDeletePair           uint16 = 0x0108 // Delete pair (smart backspace)
```

- [ ] **Step 3: 在 binary_codec.go 中新增编码方法**

在 `EncodeCommitText` 方法之后添加：

```go
// EncodeCommitTextWithCursor 编码带光标偏移的文本插入响应
// Format: textLength(4) + cursorOffset(4) + UTF-8 text
func (c *BinaryCodec) EncodeCommitTextWithCursor(text string, cursorOffset int) []byte {
	textBytes := []byte(text)
	payloadLen := uint32(8 + len(textBytes))
	header := c.EncodeHeader(CmdCommitTextWithCursor, payloadLen)

	payload := make([]byte, 8)
	binary.LittleEndian.PutUint32(payload[0:4], uint32(len(textBytes)))
	binary.LittleEndian.PutUint32(payload[4:8], uint32(cursorOffset))

	result := make([]byte, 0, HeaderSize+payloadLen)
	result = append(result, header...)
	result = append(result, payload...)
	result = append(result, textBytes...)
	return result
}

// EncodeMoveCursor 编码光标移动响应（智能跳过）
// Format: direction(4) — 1=right
func (c *BinaryCodec) EncodeMoveCursor(direction int) []byte {
	payloadLen := uint32(4)
	header := c.EncodeHeader(CmdMoveCursor, payloadLen)

	payload := make([]byte, 4)
	binary.LittleEndian.PutUint32(payload[0:4], uint32(direction))

	result := make([]byte, 0, HeaderSize+payloadLen)
	result = append(result, header...)
	result = append(result, payload...)
	return result
}

// EncodeDeletePair 编码配对删除响应（智能删除）
// Format: no payload (fixed behavior: delete 1 char left + 1 char right)
func (c *BinaryCodec) EncodeDeletePair() []byte {
	return c.EncodeHeader(CmdDeletePair, 0)
}
```

- [ ] **Step 4: 在 server_handler.go 的 handleKeyEvent 中添加新响应类型分发**

在 `switch result.Type` 的 `case ResponseTypeConsumed:` 之后添加：

```go
case ResponseTypeInsertTextWithCursor:
	s.logger.Debug("Returning CommitTextWithCursor response", "clientID", clientID,
		"cursorOffset", result.CursorOffset)
	return s.codec.EncodeCommitTextWithCursor(result.Text, result.CursorOffset)

case ResponseTypeMoveCursorRight:
	s.logger.Debug("Returning MoveCursorRight response", "clientID", clientID)
	return s.codec.EncodeMoveCursor(1)

case ResponseTypeDeletePair:
	s.logger.Debug("Returning DeletePair response", "clientID", clientID)
	return s.codec.EncodeDeletePair()
```

- [ ] **Step 5: 格式化并编译确认**

Run: `cd D:/Develop/workspace/go_dev/WindInput/wind_input && go fmt ./internal/bridge/ ./internal/ipc/ && go build ./...`
Expected: 无错误

---

### Task 4: 集成 PairTracker 到 Coordinator

**Files:**
- Modify: `wind_input/internal/coordinator/coordinator.go`
- Modify: `wind_input/internal/coordinator/handle_punctuation.go`
- Modify: `wind_input/internal/coordinator/handle_key_action.go`
- Modify: `wind_input/internal/coordinator/handle_key_event.go`
- Modify: `wind_input/internal/coordinator/handle_lifecycle.go`
- Modify: `wind_input/internal/coordinator/handle_config.go`

- [ ] **Step 1: 在 coordinator.go 中添加 pairTracker 字段**

在 `punctConverter` 字段之后添加：

```go
// Auto-pair tracker for bracket pairing (push on insert, pop on skip/delete)
pairTracker *transform.PairTracker
```

在 `NewCoordinator` 中 `punctConverter` 初始化之后添加：

```go
pairTracker: transform.NewPairTracker(cfg.Input.AutoPair.Pairs),
```

在 `clearState()` 方法中添加清空配对栈（在 `c.engineMgr.InvalidateCommandCache()` 之前）：

```go
// 清空配对栈（输入状态重置意味着光标位置不再可预测）
if c.pairTracker != nil {
	c.pairTracker.Clear()
}
```

- [ ] **Step 2: 在 handle_punctuation.go 中实现自动配对和智能跳过**

在 `handlePunctuation` 方法末尾（return 语句之前），插入自动配对逻辑。修改"No input buffer - directly handle punctuation"分支。

将现有的直通标点返回逻辑替换为包含配对判断的版本。具体修改 `handlePunctuation` 方法中 `// No input buffer - directly handle punctuation` 之后的代码块：

**原始代码（第176-199行）：**
```go
// No input buffer - directly handle punctuation
punctText := string(r)
if c.chinesePunctuation {
	// 数字后智能标点...
	if c.shouldSmartPunct(r, afterDigit, prevChar) {
		punctText = string(r)
	} else {
		var converted bool
		punctText, converted = c.punctConverter.ToChinesePunctStr(r)
		if !converted {
			punctText = string(r)
		}
	}
}

// Apply full-width conversion if enabled
if c.fullWidth {
	punctText = transform.ToFullWidth(punctText)
}

return &bridge.KeyEventResult{
	Type: bridge.ResponseTypeInsertText,
	Text: punctText,
}
```

**替换为：**
```go
// No input buffer - directly handle punctuation
punctText := string(r)
if c.chinesePunctuation {
	if c.shouldSmartPunct(r, afterDigit, prevChar) {
		punctText = string(r)
	} else {
		var converted bool
		punctText, converted = c.punctConverter.ToChinesePunctStr(r)
		if !converted {
			punctText = string(r)
		}
	}
}

// Apply full-width conversion if enabled
if c.fullWidth {
	punctText = transform.ToFullWidth(punctText)
}

// 自动配对：检查转换后的标点是否需要配对
if c.shouldAutoPair() {
	punctRunes := []rune(punctText)
	if len(punctRunes) == 1 {
		// 智能跳过：输入右标点时，如果栈顶匹配则跳过
		if c.pairTracker.IsRight(punctRunes[0]) {
			if entry, ok := c.pairTracker.Peek(); ok && entry.Right == punctRunes[0] {
				c.pairTracker.Pop()
				c.logger.Debug("Auto-pair: smart skip", "char", punctText)
				return &bridge.KeyEventResult{
					Type: bridge.ResponseTypeMoveCursorRight,
				}
			}
			// 栈顶不匹配，清空栈
			c.pairTracker.Clear()
		}

		// 自动配对：输入左标点时，插入配对并回退光标
		if right, ok := c.pairTracker.GetRight(punctRunes[0]); ok {
			pairText := punctText + string(right)
			c.pairTracker.Push(punctRunes[0], right)
			c.logger.Debug("Auto-pair: insert pair", "text", pairText)
			return &bridge.KeyEventResult{
				Type:         bridge.ResponseTypeInsertTextWithCursor,
				Text:         pairText,
				CursorOffset: 1,
			}
		}
	}
}

return &bridge.KeyEventResult{
	Type: bridge.ResponseTypeInsertText,
	Text: punctText,
}
```

同时在 `handlePunctuation` 的 `punct_commit` 分支（第121-168行）中，在 `c.clearState()` 之后添加（clearState 已经清空了 pairTracker，但 punct_commit 后接着输出标点也需要判断配对）：

修改 punct_commit 分支中最后的 return 语句。将 `return &bridge.KeyEventResult{Type: bridge.ResponseTypeInsertText, Text: prefix + text + punctText}` 改为：

```go
commitText := prefix + text

// punct_commit 后的标点也支持自动配对
if c.shouldAutoPair() {
	punctRunes := []rune(punctText)
	if len(punctRunes) == 1 {
		if right, ok := c.pairTracker.GetRight(punctRunes[0]); ok {
			pairPunctText := punctText + string(right)
			c.pairTracker.Push(punctRunes[0], right)
			c.logger.Debug("Auto-pair: insert pair after punct_commit", "text", pairPunctText)
			return &bridge.KeyEventResult{
				Type:         bridge.ResponseTypeInsertTextWithCursor,
				Text:         commitText + pairPunctText,
				CursorOffset: 1,
			}
		}
	}
}

return &bridge.KeyEventResult{
	Type: bridge.ResponseTypeInsertText,
	Text: commitText + punctText,
}
```

- [ ] **Step 3: 添加 shouldAutoPair 辅助方法**

在 `handle_punctuation.go` 中添加辅助方法：

```go
// shouldAutoPair 判断当前是否应启用自动配对
func (c *Coordinator) shouldAutoPair() bool {
	if c.config == nil || !c.config.Input.AutoPair.Enabled {
		return false
	}
	if !c.chineseMode || !c.chinesePunctuation {
		return false
	}
	if c.pairTracker == nil {
		return false
	}
	// 检查应用黑名单
	if len(c.config.Input.AutoPair.Blacklist) > 0 && c.activeProcessName != "" {
		for _, proc := range c.config.Input.AutoPair.Blacklist {
			if strings.EqualFold(proc, c.activeProcessName) {
				return false
			}
		}
	}
	return true
}
```

- [ ] **Step 4: 在 handleBackspace 中添加智能删除**

修改 `handle_key_action.go` 中 `handleBackspace` 方法。在方法末尾现有的空缓冲区 PassThrough 逻辑之前（第286-289行），添加配对删除判断：

**原始代码：**
```go
if len(c.inputBuffer) == 0 {
	// Buffer is already empty and no confirmed segments - pass through to system
	c.logger.Debug("Backspace with empty buffer, passing through to system")
	return nil
}
```

**替换为：**
```go
if len(c.inputBuffer) == 0 {
	// 智能删除：如果配对栈非空，删除光标左右各一个字符
	if c.pairTracker != nil && !c.pairTracker.IsEmpty() {
		c.pairTracker.Pop()
		c.logger.Debug("Auto-pair: smart delete pair")
		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeDeletePair,
		}
	}
	// Buffer is already empty and no confirmed segments - pass through to system
	c.logger.Debug("Backspace with empty buffer, passing through to system")
	return nil
}
```

- [ ] **Step 5: 在 handle_key_event.go 中添加配对栈清空时机**

在 `HandleKeyEvent` 方法中，需要在以下位置清空配对栈：

**5a. 按方向键时清空** — 在 `case vk == ipc.VK_LEFT:` 分支之前，添加对方向键的栈清空：

在 `switch` 语句的以下 case 中（VK_LEFT, VK_RIGHT, VK_HOME, VK_END），各自的处理函数调用之前不方便添加，改为在各方向键 handler 内部添加。

实际上更简洁的做法：在 `handleCursorLeft`, `handleCursorRight`, `handleCursorHome`, `handleCursorEnd` 每个方法开头添加 `c.clearPairStack()`。

但为了避免过多改动，我们在 `HandleKeyEvent` 的 switch 之前添加一个通用的栈清空判断：

在 `switch` 语句（`// Chinese mode handling` 注释之后）之前，添加：

```go
// 自动配对：方向键、Enter、Escape 等清空配对栈
if c.pairTracker != nil {
	switch vk {
	case ipc.VK_LEFT, ipc.VK_RIGHT, ipc.VK_UP, ipc.VK_DOWN,
		ipc.VK_HOME, ipc.VK_END, ipc.VK_RETURN, ipc.VK_ESCAPE:
		c.pairTracker.Clear()
	}
}
```

**5b. 普通字符输入时清空** — 在 `handleAlphaKey` 方法开头（`handle_key_action.go`），`c.lastKeyTime = startTime` 之后添加：

```go
// 输入字母时清空配对栈（光标和配对之间插入了内容）
if c.pairTracker != nil {
	c.pairTracker.Clear()
}
```

**5c. 数字键直通时清空** — 在 `HandleKeyEvent` 中数字键处理的 `c.lastOutputWasDigit = true` 之前添加：

```go
if c.pairTracker != nil {
	c.pairTracker.Clear()
}
```

**5d. 小键盘输出时清空** — 在小键盘处理分支 `text := numpadChar` 之前添加：

```go
if c.pairTracker != nil {
	c.pairTracker.Clear()
}
```

- [ ] **Step 6: 焦点丢失和模式切换时清空配对栈**

在 `handle_lifecycle.go` 的 `HandleFocusLost` 方法中（`c.clearState()` 已经会清空栈，无需额外操作）。

在 `HandleSelectionChanged` 方法中添加清空（在方法体 `c.mu.Lock()` 后面的逻辑中添加）：

```go
// 选区变化时清空配对栈
if c.pairTracker != nil {
	c.pairTracker.Clear()
}
```

在 `handle_punctuation.go` 的 `applyTogglePunct` 方法中（`c.punctConverter.Reset()` 之后）添加：

```go
if c.pairTracker != nil {
	c.pairTracker.Clear()
}
```

- [ ] **Step 7: 在 handle_config.go 的 UpdateInputConfig 中支持热更新**

在 `UpdateInputConfig` 方法中 `c.config.Input = *inputConfig` 之后添加：

```go
// 更新自动配对配置
if c.pairTracker != nil {
	c.pairTracker.UpdatePairs(inputConfig.AutoPair.Pairs)
}
```

- [ ] **Step 8: 格式化并编译确认**

Run: `cd D:/Develop/workspace/go_dev/WindInput/wind_input && go fmt ./internal/coordinator/ && go build ./...`
Expected: 无错误

---

### Task 5: 扩展 C++ 层二进制协议和响应解析

**Files:**
- Modify: `wind_tsf/include/BinaryProtocol.h`
- Modify: `wind_tsf/src/IPCClient.cpp`

- [ ] **Step 1: 在 BinaryProtocol.h 中添加新命令和类型**

在 `CMD_CONSUMED` 之后添加新命令常量：

```cpp
constexpr uint16_t CMD_COMMIT_TEXT_WITH_CURSOR = 0x0106; // Commit text with cursor offset
constexpr uint16_t CMD_MOVE_CURSOR             = 0x0107; // Move cursor (smart skip)
constexpr uint16_t CMD_DELETE_PAIR             = 0x0108; // Delete pair (smart backspace)
```

在 `ResponseType` 枚举中 `Error` 之前添加：

```cpp
InsertTextWithCursor, // Insert text and position cursor
MoveCursorRight,      // Move cursor right (smart skip)
DeletePair,           // Delete left + right char (smart delete)
```

在 `ParsedResponse` 结构体中添加字段（`caretPos` 之后）：

```cpp
int cursorOffset = 0;  // For InsertTextWithCursor: chars to move left from end
```

新增 payload 结构体（在 `#pragma pack(push, 1)` 内，`CommitTextHeader` 之后）：

```cpp
// Commit text with cursor payload
struct CommitTextWithCursorPayload
{
    uint32_t textLength;    // Length of text (UTF-8)
    uint32_t cursorOffset;  // Chars to move left from end of inserted text
    // Followed by UTF-8 text
};
static_assert(sizeof(CommitTextWithCursorPayload) == 8, "CommitTextWithCursorPayload must be 8 bytes");

// Move cursor payload
struct MoveCursorPayload
{
    uint32_t direction; // 1=right
};
static_assert(sizeof(MoveCursorPayload) == 4, "MoveCursorPayload must be 4 bytes");
```

- [ ] **Step 2: 在 IPCClient.cpp 的 _ParseResponse 中处理新命令**

在 `case CMD_HOST_RENDER_SETUP:` 之前添加三个新 case：

```cpp
case CMD_COMMIT_TEXT_WITH_CURSOR:
    {
        response.type = ResponseType::InsertTextWithCursor;
        if (payload.size() < sizeof(CommitTextWithCursorPayload))
        {
            _LogError(L"CommitTextWithCursor payload too short");
            return FALSE;
        }
        const CommitTextWithCursorPayload* p = reinterpret_cast<const CommitTextWithCursorPayload*>(payload.data());
        response.cursorOffset = (int)p->cursorOffset;
        if (p->textLength > 0)
        {
            size_t textOffset = sizeof(CommitTextWithCursorPayload);
            if (textOffset + p->textLength <= payload.size())
            {
                std::string utf8(reinterpret_cast<const char*>(payload.data() + textOffset), p->textLength);
                response.text = Utf8ToWide(utf8);
            }
        }
        _LogDebug(L"Response: InsertTextWithCursor textLen=%zu, cursorOffset=%d",
                  response.text.length(), response.cursorOffset);
    }
    break;

case CMD_MOVE_CURSOR:
    {
        response.type = ResponseType::MoveCursorRight;
        _LogDebug(L"Response: MoveCursorRight");
    }
    break;

case CMD_DELETE_PAIR:
    {
        response.type = ResponseType::DeletePair;
        _LogDebug(L"Response: DeletePair");
    }
    break;
```

- [ ] **Step 3: 编译确认 C++ 侧**

Run: `cd D:/Develop/workspace/go_dev/WindInput && cmake --build build --config Release --target wind_tsf 2>&1 | tail -5`
Expected: 编译成功（或根据项目构建系统调整命令）

---

### Task 6: 实现 C++ 层 EditSession 和响应处理

**Files:**
- Modify: `wind_tsf/src/TextService.cpp`
- Modify: `wind_tsf/src/TextService.h`（如果需要声明新方法）
- Modify: `wind_tsf/src/KeyEventSink.cpp`

- [ ] **Step 1: 在 TextService.cpp 中添加 CInsertTextWithCursorEditSession 类**

在 `CCommitTextEditSession` 类之后添加新的 EditSession 类：

```cpp
// EditSession for inserting text and positioning cursor at an offset from the end
class CInsertTextWithCursorEditSession : public ITfEditSession
{
public:
    CInsertTextWithCursorEditSession(CTextService* pTextService, ITfContext* pContext,
                                      const std::wstring& text, int cursorOffset)
        : _refCount(1), _pTextService(pTextService), _pContext(pContext),
          _text(text), _cursorOffset(cursorOffset)
    {
        _pTextService->AddRef();
        _pContext->AddRef();
    }

    ~CInsertTextWithCursorEditSession()
    {
        _pTextService->Release();
        _pContext->Release();
    }

    // IUnknown
    STDMETHODIMP QueryInterface(REFIID riid, void** ppvObj)
    {
        if (ppvObj == nullptr) return E_INVALIDARG;
        *ppvObj = nullptr;
        if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITfEditSession))
            *ppvObj = static_cast<ITfEditSession*>(this);
        if (*ppvObj) { AddRef(); return S_OK; }
        return E_NOINTERFACE;
    }
    STDMETHODIMP_(ULONG) AddRef() { return InterlockedIncrement(&_refCount); }
    STDMETHODIMP_(ULONG) Release()
    {
        LONG cr = InterlockedDecrement(&_refCount);
        if (cr == 0) delete this;
        return cr;
    }

    STDMETHODIMP DoEditSession(TfEditCookie ec)
    {
        // End any existing composition first
        ITfComposition* pComp = _pTextService->GetActiveComposition();
        if (pComp != nullptr)
        {
            pComp->EndComposition(ec);
            _pTextService->SetActiveComposition(nullptr);
        }

        // Insert text at selection
        ITfInsertAtSelection* pInsertAtSel = nullptr;
        HRESULT hr = _pContext->QueryInterface(IID_ITfInsertAtSelection, (void**)&pInsertAtSel);
        if (FAILED(hr) || pInsertAtSel == nullptr)
        {
            WIND_LOG_ERROR(L"InsertTextWithCursor: Failed to get ITfInsertAtSelection\n");
            return E_FAIL;
        }

        ITfRange* pRange = nullptr;
        hr = pInsertAtSel->InsertTextAtSelection(ec, 0, _text.c_str(), (LONG)_text.length(), &pRange);
        pInsertAtSel->Release();

        if (FAILED(hr) || pRange == nullptr)
        {
            WIND_LOG_ERROR(L"InsertTextWithCursor: InsertTextAtSelection failed\n");
            return hr;
        }

        // Move cursor: collapse to end, then shift left by cursorOffset
        pRange->Collapse(ec, TF_ANCHOR_END);

        if (_cursorOffset > 0)
        {
            LONG shifted = 0;
            pRange->ShiftStart(ec, -_cursorOffset, &shifted, nullptr);
            pRange->ShiftEnd(ec, -_cursorOffset, &shifted, nullptr);
        }

        // Set selection to the new position
        TF_SELECTION sel;
        sel.range = pRange;
        sel.style.ase = TF_AE_NONE;
        sel.style.fInterimChar = FALSE;
        _pContext->SetSelection(ec, 1, &sel);

        pRange->Release();

        WIND_LOG_DEBUG_FMT(L"InsertTextWithCursor: inserted %zu chars, cursorOffset=%d\n",
                          _text.length(), _cursorOffset);
        return S_OK;
    }

private:
    LONG _refCount;
    CTextService* _pTextService;
    ITfContext* _pContext;
    std::wstring _text;
    int _cursorOffset;
};
```

- [ ] **Step 2: 添加 CMoveCursorEditSession 类**

```cpp
// EditSession for moving cursor right by 1 character (smart skip)
class CMoveCursorEditSession : public ITfEditSession
{
public:
    CMoveCursorEditSession(ITfContext* pContext)
        : _refCount(1), _pContext(pContext)
    {
        _pContext->AddRef();
    }

    ~CMoveCursorEditSession() { _pContext->Release(); }

    STDMETHODIMP QueryInterface(REFIID riid, void** ppvObj)
    {
        if (ppvObj == nullptr) return E_INVALIDARG;
        *ppvObj = nullptr;
        if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITfEditSession))
            *ppvObj = static_cast<ITfEditSession*>(this);
        if (*ppvObj) { AddRef(); return S_OK; }
        return E_NOINTERFACE;
    }
    STDMETHODIMP_(ULONG) AddRef() { return InterlockedIncrement(&_refCount); }
    STDMETHODIMP_(ULONG) Release()
    {
        LONG cr = InterlockedDecrement(&_refCount);
        if (cr == 0) delete this;
        return cr;
    }

    STDMETHODIMP DoEditSession(TfEditCookie ec)
    {
        TF_SELECTION sel[1];
        ULONG fetched = 0;
        HRESULT hr = _pContext->GetSelection(ec, TF_DEFAULT_SELECTION, 1, sel, &fetched);
        if (FAILED(hr) || fetched == 0 || sel[0].range == nullptr)
        {
            WIND_LOG_ERROR(L"MoveCursor: Failed to get selection\n");
            return E_FAIL;
        }

        // Move range right by 1
        LONG shifted = 0;
        sel[0].range->ShiftEnd(ec, 1, &shifted, nullptr);
        sel[0].range->Collapse(ec, TF_ANCHOR_END);

        sel[0].style.ase = TF_AE_NONE;
        sel[0].style.fInterimChar = FALSE;
        _pContext->SetSelection(ec, 1, sel);
        sel[0].range->Release();

        WIND_LOG_DEBUG(L"MoveCursor: moved right by 1\n");
        return S_OK;
    }

private:
    LONG _refCount;
    ITfContext* _pContext;
};
```

- [ ] **Step 3: 添加 CDeletePairEditSession 类**

```cpp
// EditSession for deleting a pair (1 char left + 1 char right of cursor)
class CDeletePairEditSession : public ITfEditSession
{
public:
    CDeletePairEditSession(ITfContext* pContext)
        : _refCount(1), _pContext(pContext)
    {
        _pContext->AddRef();
    }

    ~CDeletePairEditSession() { _pContext->Release(); }

    STDMETHODIMP QueryInterface(REFIID riid, void** ppvObj)
    {
        if (ppvObj == nullptr) return E_INVALIDARG;
        *ppvObj = nullptr;
        if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITfEditSession))
            *ppvObj = static_cast<ITfEditSession*>(this);
        if (*ppvObj) { AddRef(); return S_OK; }
        return E_NOINTERFACE;
    }
    STDMETHODIMP_(ULONG) AddRef() { return InterlockedIncrement(&_refCount); }
    STDMETHODIMP_(ULONG) Release()
    {
        LONG cr = InterlockedDecrement(&_refCount);
        if (cr == 0) delete this;
        return cr;
    }

    STDMETHODIMP DoEditSession(TfEditCookie ec)
    {
        TF_SELECTION sel[1];
        ULONG fetched = 0;
        HRESULT hr = _pContext->GetSelection(ec, TF_DEFAULT_SELECTION, 1, sel, &fetched);
        if (FAILED(hr) || fetched == 0 || sel[0].range == nullptr)
        {
            WIND_LOG_ERROR(L"DeletePair: Failed to get selection\n");
            return E_FAIL;
        }

        // Expand selection: 1 char left + 1 char right
        LONG shifted = 0;
        sel[0].range->ShiftStart(ec, -1, &shifted, nullptr);
        sel[0].range->ShiftEnd(ec, 1, &shifted, nullptr);

        // Delete by setting empty text
        hr = sel[0].range->SetText(ec, 0, L"", 0);
        if (FAILED(hr))
        {
            WIND_LOG_ERROR(L"DeletePair: SetText failed\n");
            sel[0].range->Release();
            return hr;
        }

        // Collapse to set cursor position
        sel[0].range->Collapse(ec, TF_ANCHOR_START);
        sel[0].style.ase = TF_AE_NONE;
        sel[0].style.fInterimChar = FALSE;
        _pContext->SetSelection(ec, 1, sel);
        sel[0].range->Release();

        WIND_LOG_DEBUG(L"DeletePair: deleted pair\n");
        return S_OK;
    }

private:
    LONG _refCount;
    ITfContext* _pContext;
};
```

- [ ] **Step 4: 在 TextService 中添加公共方法**

在 TextService.cpp 中添加（或在已有的 `InsertText`/`CommitText` 方法附近）：

```cpp
BOOL CTextService::InsertTextWithCursor(const std::wstring& text, int cursorOffset)
{
    if (text.empty()) return TRUE;
    if (_pThreadMgr == nullptr) return FALSE;

    ITfDocumentMgr* pDocMgr = nullptr;
    HRESULT hr = _pThreadMgr->GetFocus(&pDocMgr);
    if (FAILED(hr) || pDocMgr == nullptr) return FALSE;

    ITfContext* pContext = nullptr;
    hr = pDocMgr->GetTop(&pContext);
    pDocMgr->Release();
    if (FAILED(hr) || pContext == nullptr) return FALSE;

    CInsertTextWithCursorEditSession* pEditSession =
        new CInsertTextWithCursorEditSession(this, pContext, text, cursorOffset);

    HRESULT hrSession = S_OK;
    hr = pContext->RequestEditSession(_tfClientId, pEditSession,
                                      TF_ES_SYNC | TF_ES_READWRITE, &hrSession);
    pEditSession->Release();
    pContext->Release();

    return SUCCEEDED(hr) && SUCCEEDED(hrSession);
}

BOOL CTextService::MoveCursorRight()
{
    if (_pThreadMgr == nullptr) return FALSE;

    ITfDocumentMgr* pDocMgr = nullptr;
    HRESULT hr = _pThreadMgr->GetFocus(&pDocMgr);
    if (FAILED(hr) || pDocMgr == nullptr) return FALSE;

    ITfContext* pContext = nullptr;
    hr = pDocMgr->GetTop(&pContext);
    pDocMgr->Release();
    if (FAILED(hr) || pContext == nullptr) return FALSE;

    CMoveCursorEditSession* pEditSession = new CMoveCursorEditSession(pContext);

    HRESULT hrSession = S_OK;
    hr = pContext->RequestEditSession(_tfClientId, pEditSession,
                                      TF_ES_SYNC | TF_ES_READWRITE, &hrSession);
    pEditSession->Release();
    pContext->Release();

    return SUCCEEDED(hr) && SUCCEEDED(hrSession);
}

BOOL CTextService::DeletePair()
{
    if (_pThreadMgr == nullptr) return FALSE;

    ITfDocumentMgr* pDocMgr = nullptr;
    HRESULT hr = _pThreadMgr->GetFocus(&pDocMgr);
    if (FAILED(hr) || pDocMgr == nullptr) return FALSE;

    ITfContext* pContext = nullptr;
    hr = pDocMgr->GetTop(&pContext);
    pDocMgr->Release();
    if (FAILED(hr) || pContext == nullptr) return FALSE;

    CDeletePairEditSession* pEditSession = new CDeletePairEditSession(pContext);

    HRESULT hrSession = S_OK;
    hr = pContext->RequestEditSession(_tfClientId, pEditSession,
                                      TF_ES_SYNC | TF_ES_READWRITE, &hrSession);
    pEditSession->Release();
    pContext->Release();

    return SUCCEEDED(hr) && SUCCEEDED(hrSession);
}
```

在 TextService.h 中声明这三个方法：

```cpp
BOOL InsertTextWithCursor(const std::wstring& text, int cursorOffset);
BOOL MoveCursorRight();
BOOL DeletePair();
```

- [ ] **Step 5: 在 KeyEventSink.cpp 的 _HandleServiceResponse 中添加新 case**

在 `case ResponseType::Consumed:` 之后添加：

```cpp
case ResponseType::InsertTextWithCursor:
    {
        WIND_LOG_DEBUG(L"Processing InsertTextWithCursor response\n");
        _pTextService->InsertTextWithCursor(response.text, response.cursorOffset);
        _isComposing = FALSE;
        _hasCandidates = FALSE;
    }
    return TRUE;

case ResponseType::MoveCursorRight:
    {
        WIND_LOG_DEBUG(L"Processing MoveCursorRight response (smart skip)\n");
        _pTextService->MoveCursorRight();
    }
    return TRUE;

case ResponseType::DeletePair:
    {
        WIND_LOG_DEBUG(L"Processing DeletePair response (smart delete)\n");
        _pTextService->DeletePair();
    }
    return TRUE;
```

- [ ] **Step 6: 编译确认 C++ 侧**

Run: 根据项目构建系统编译 wind_tsf
Expected: 编译成功

---

### Task 7: 全项目构建验证

**Files:** 无新文件

- [ ] **Step 1: 编译 Go 侧**

Run: `cd D:/Develop/workspace/go_dev/WindInput/wind_input && go fmt ./... && go build ./...`
Expected: 无错误

- [ ] **Step 2: 运行 Go 单元测试**

Run: `cd D:/Develop/workspace/go_dev/WindInput/wind_input && go test ./internal/transform/ -v`
Expected: 所有 PairTracker 测试通过

- [ ] **Step 3: 编译 C++ 侧**

Run: 根据项目构建系统编译 wind_tsf
Expected: 编译成功

- [ ] **Step 4: 检查 IPC 协议一致性**

手动确认以下常量在 Go 和 C++ 两侧一致：
- `CMD_COMMIT_TEXT_WITH_CURSOR = 0x0106`
- `CMD_MOVE_CURSOR = 0x0107`
- `CMD_DELETE_PAIR = 0x0108`
- `CommitTextWithCursorPayload` 的布局: textLength(4) + cursorOffset(4)
