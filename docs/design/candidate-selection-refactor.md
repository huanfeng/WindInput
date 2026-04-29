# 候选词选择统一重构方案

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将三套平行的候选词选择实现合并为单一 `doSelectCandidate` 核心，消除重复逻辑，使 InlinePreedit 等配置检查只存在于一处。

**Architecture:** 提取 `doSelectCandidate(index int) *bridge.KeyEventResult` 作为唯一核心实现，包含全部功能（组候选展开、分步确认、完整上屏、学习回调、历史记录、统计）。原 `selectCandidate` 成为直通 wrapper；`selectCandidateInternal` 删除，其调用方改用 `doSelectCandidate`；鼠标回调通过 `pushKeyEventResult` 辅助函数将返回值转为 push 调用。新增 `compositionUpdateResult()` 作为 InlinePreedit 检查的唯一入口。

**Tech Stack:** Go，`wind_input/internal/coordinator`，`wind_input/internal/bridge`

---

## 背景：现有问题

当前代码有三套平行实现，共享 95% 逻辑，但各有遗漏：

| 功能 | `selectCandidate` | `selectCandidateInternal` | `handleCandidateSelect`（鼠标） |
|---|---|---|---|
| inputHistory.Record | ✓ | **✗ 漏了** | ✓ |
| recordCommit（统计） | ✓ | ✓ | **✗ 漏了** |
| 组候选展开 | ✓ | ✓ | **✗ 漏了** |
| InlinePreedit 检查 | ✓ | ✓ | ✓ |

每次新增 InlinePreedit 这样的配置逻辑，需要改 N 处且容易遗漏。

---

## 文件改动范围

| 文件 | 操作 | 说明 |
|---|---|---|
| `internal/coordinator/handle_candidates.go` | 新增函数 | `compositionUpdateResult()`、`doSelectCandidate()` |
| `internal/coordinator/handle_key_action.go` | 简化 | `selectCandidate` 变为 1 行 wrapper |
| `internal/coordinator/handle_lifecycle.go` | 删除+修改调用方 | 删除 `selectCandidateInternal`，改调 `doSelectCandidate` |
| `internal/coordinator/handle_ui_callbacks.go` | 重写 | `handleCandidateSelect` 用 `doSelectCandidate` + push helper |

---

## Task 1：新增 `compositionUpdateResult()` 辅助函数

**Files:**
- Modify: `wind_input/internal/coordinator/handle_candidates.go`

InlinePreedit 检查的**唯一入口**。当前代码中同样的 if/else 出现了 5 次，重构后收敛到这一处。

- [ ] **Step 1: 在 `handle_candidates.go` 末尾添加函数**

在文件末尾（`displayCursorPos` 之后）添加：

```go
// compositionUpdateResult 构建 UpdateComposition 响应，遵循 InlinePreedit 配置：
// 关闭时发送空文本，避免编码嵌入应用与候选窗同时显示。
func (c *Coordinator) compositionUpdateResult() *bridge.KeyEventResult {
	if c.config != nil && !c.config.UI.InlinePreedit {
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeUpdateComposition}
	}
	return &bridge.KeyEventResult{
		Type:     bridge.ResponseTypeUpdateComposition,
		Text:     c.compositionText(),
		CaretPos: c.displayCursorPos(),
	}
}
```

- [ ] **Step 2: 编译确认无误**

```bash
cd wind_input && go build ./...
```

Expected: 无输出（编译通过）

- [ ] **Step 3: 格式化**

```bash
go fmt ./internal/coordinator/handle_candidates.go
```

---

## Task 2：实现 `doSelectCandidate()` 核心

**Files:**
- Modify: `wind_input/internal/coordinator/handle_candidates.go`

合并三套实现的全部功能：组候选展开、分步确认、完整上屏（含学习回调、历史记录、统计）。

- [ ] **Step 1: 在 `handle_candidates.go` 末尾添加 `doSelectCandidate`**

需要在文件顶部 import 中确认以下包已引入（当前文件已有则无需新增）：
- `"strings"`
- `"github.com/huanfeng/wind_input/internal/bridge"`
- `"github.com/huanfeng/wind_input/internal/engine"`
- `"github.com/huanfeng/wind_input/internal/store"`
- `"github.com/huanfeng/wind_input/internal/transform"`

添加函数（置于 `compositionUpdateResult` 之后）：

```go
// doSelectCandidate 是候选词选择的统一核心实现（调用方须持锁）。
// 处理组候选展开、拼音分步确认、完整上屏三种情形，
// 包含学习回调、输入历史记录和统计上报，返回需交付给 TSF 的结果。
func (c *Coordinator) doSelectCandidate(index int) *bridge.KeyEventResult {
	if index < 0 || index >= len(c.candidates) {
		return nil
	}
	cand := c.candidates[index]
	c.logger.Debug("Candidate selected", "index", index)

	// ── 组候选：替换 inputBuffer 为组的完整编码，触发二级展开 ──────────────
	if cand.IsGroup && cand.GroupCode != "" {
		c.inputBuffer = cand.GroupCode
		c.inputCursorPos = len(c.inputBuffer)
		c.currentPage = 1
		c.selectedIndex = 0
		c.updateCandidates()
		c.showUI()
		return c.compositionUpdateResult()
	}

	originalText := cand.Text
	text := originalText
	if c.fullWidth {
		text = transform.ToFullWidth(text)
	}

	isPinyin := c.engineMgr != nil && c.engineMgr.GetCurrentType() == engine.EngineTypePinyin
	isMixed := c.engineMgr != nil && c.engineMgr.GetCurrentType() == engine.EngineTypeMixed

	// ── 拼音分步确认：候选消耗长度 < 缓冲区长度，暂存已确认段 ──────────────
	if (isPinyin || (isMixed && cand.ConsumedLength > 0)) &&
		cand.ConsumedLength > 0 && cand.ConsumedLength < len(c.inputBuffer) {

		consumedCode := c.inputBuffer[:cand.ConsumedLength]
		if !cand.IsCommand {
			c.engineMgr.OnCandidateSelected(consumedCode, originalText, cand.Source)
		}

		remaining := c.inputBuffer[cand.ConsumedLength:]
		c.logger.Debug("Partial confirm (pinyin)", "index", index, "text", text,
			"consumed", cand.ConsumedLength, "remaining", remaining,
			"confirmedCount", len(c.confirmedSegments)+1)

		c.confirmedSegments = append(c.confirmedSegments, ConfirmedSegment{
			Text:         originalText,
			ConsumedCode: consumedCode,
		})
		c.inputBuffer = remaining
		c.inputCursorPos = len(remaining)
		c.currentPage = 1
		c.updateCandidates()
		c.showUI()
		return c.compositionUpdateResult()
	}

	// ── 完全消费：学习回调 ────────────────────────────────────────────────
	if c.engineMgr != nil && !cand.IsCommand {
		if (isPinyin || isMixed) && len(c.confirmedSegments) > 0 {
			var fullCode, fullText strings.Builder
			for _, seg := range c.confirmedSegments {
				fullCode.WriteString(seg.ConsumedCode)
				fullText.WriteString(seg.Text)
			}
			fullCode.WriteString(c.inputBuffer)
			fullText.WriteString(originalText)
			c.engineMgr.OnCandidateSelected(fullCode.String(), fullText.String(), cand.Source)
		} else {
			selectedCode := c.inputBuffer
			if cand.Code != "" {
				selectedCode = cand.Code
			}
			c.engineMgr.OnCandidateSelected(selectedCode, originalText, cand.Source)
		}
	}

	// ── 输入历史记录（用于加词推荐）────────────────────────────────────────
	if c.inputHistory != nil && !cand.IsCommand {
		histText := originalText
		histCode := c.inputBuffer
		if (isPinyin || isMixed) && len(c.confirmedSegments) > 0 {
			var fCode, fText strings.Builder
			for _, seg := range c.confirmedSegments {
				fCode.WriteString(seg.ConsumedCode)
				fText.WriteString(seg.Text)
			}
			histText = fText.String() + originalText
			histCode = fCode.String() + c.inputBuffer
		}
		c.inputHistory.Record(histText, histCode, "", 0)
	}

	// ── 拼接已确认段 + 当前候选，构建最终上屏文本 ──────────────────────────
	finalText := text
	if (isPinyin || isMixed) && len(c.confirmedSegments) > 0 {
		var sb strings.Builder
		for _, seg := range c.confirmedSegments {
			t := seg.Text
			if c.fullWidth {
				t = transform.ToFullWidth(t)
			}
			sb.WriteString(t)
		}
		finalText = sb.String() + text
	}

	c.logger.Debug("Candidate selected (full commit)", "index", index,
		"original", originalText, "output", finalText,
		"fullWidth", c.fullWidth, "confirmedSegments", len(c.confirmedSegments))

	c.recordCommit(finalText, len(c.inputBuffer), index%c.candidatesPerPage, store.SourceCandidate)
	c.clearState()
	c.hideUI()

	return &bridge.KeyEventResult{
		Type: bridge.ResponseTypeInsertText,
		Text: finalText,
	}
}
```

- [ ] **Step 2: 编译确认**

```bash
cd wind_input && go build ./...
```

Expected: 无输出

- [ ] **Step 3: 格式化**

```bash
go fmt ./internal/coordinator/handle_candidates.go
```

---

## Task 3：简化 `selectCandidate`（handle_key_action.go）

**Files:**
- Modify: `wind_input/internal/coordinator/handle_key_action.go`

原有 ~150 行实现替换为 1 行调用。

- [ ] **Step 1: 找到 `selectCandidate` 函数范围**

函数从 `func (c *Coordinator) selectCandidate(index int)` 到其结束（约 918 行），全部替换为：

```go
func (c *Coordinator) selectCandidate(index int) *bridge.KeyEventResult {
	return c.doSelectCandidate(index)
}
```

- [ ] **Step 2: 检查 import 是否有残留未使用的包**

`selectCandidate` 原来引用了 `engine`、`store`、`transform`、`strings` 等包。这些引用现在移到了 `handle_candidates.go`。检查 `handle_key_action.go` 的 import 是否有因此变为未使用的项，若有则删除。

```bash
cd wind_input && go build ./...
```

Expected: 无输出（若有 "imported and not used" 错误，删除对应 import 行）

- [ ] **Step 3: 格式化**

```bash
go fmt ./internal/coordinator/handle_key_action.go
```

---

## Task 4：删除 `selectCandidateInternal`（handle_lifecycle.go）

**Files:**
- Modify: `wind_input/internal/coordinator/handle_lifecycle.go`

`selectCandidateInternal` 与 `selectCandidate` 逻辑高度重叠且有功能遗漏，统一后删除。

- [ ] **Step 1: 将调用方改为 `doSelectCandidate`**

在 `handle_lifecycle.go` 中，将所有 `c.selectCandidateInternal(...)` 调用替换为 `c.doSelectCandidate(...)`。

用 grep 定位调用处：
```bash
grep -n "selectCandidateInternal" wind_input/internal/coordinator/handle_lifecycle.go
```

将每处 `c.selectCandidateInternal(index)` 改为 `c.doSelectCandidate(index)`。

- [ ] **Step 2: 删除 `selectCandidateInternal` 函数体**

找到函数 `func (c *Coordinator) selectCandidateInternal(index int)` 的完整定义（约 130 行），全部删除。

- [ ] **Step 3: 检查 import 残留**

```bash
cd wind_input && go build ./...
```

Expected: 无输出（同 Task 3 处理方式）

- [ ] **Step 4: 格式化**

```bash
go fmt ./internal/coordinator/handle_lifecycle.go
```

---

## Task 5：重构鼠标回调 `handleCandidateSelect`（handle_ui_callbacks.go）

**Files:**
- Modify: `wind_input/internal/coordinator/handle_ui_callbacks.go`

鼠标路径的唯一特殊性是通过 push 而非 return 交付结果（因为在 goroutine 中）。新增 `pushKeyEventResult` 辅助完成转换。

- [ ] **Step 1: 在 `handle_ui_callbacks.go` 末尾添加 push 辅助函数**

```go
// pushKeyEventResult 将 KeyEventResult 通过 bridge push 管道发送给活跃 TSF 客户端。
// 用于鼠标等异步路径（无法通过 return 交付结果）。
func pushKeyEventResult(srv BridgeServer, result *bridge.KeyEventResult) {
	if result == nil || srv == nil {
		return
	}
	switch result.Type {
	case bridge.ResponseTypeInsertText:
		srv.PushCommitTextToActiveClient(result.Text)
	case bridge.ResponseTypeUpdateComposition:
		srv.PushUpdateCompositionToActiveClient(result.Text, result.CaretPos)
	case bridge.ResponseTypeClearComposition:
		srv.PushClearCompositionToActiveClient()
	}
}
```

- [ ] **Step 2: 重写 `handleCandidateSelect`**

将现有 ~140 行实现替换为：

```go
// handleCandidateSelect 处理鼠标点击选词（在独立 goroutine 中调用，通过 push 管道交付结果）
func (c *Coordinator) handleCandidateSelect(index int) {
	c.mu.Lock()

	actualIndex := (c.currentPage-1)*c.candidatesPerPage + index
	c.logger.Debug("Candidate selected via mouse", "pageIndex", index, "actualIndex", actualIndex)

	result := c.doSelectCandidate(actualIndex)
	bridgeServer := c.bridgeServer
	c.mu.Unlock()

	pushKeyEventResult(bridgeServer, result)
}
```

- [ ] **Step 3: 编译确认**

```bash
cd wind_input && go build ./...
```

Expected: 无输出

- [ ] **Step 4: 格式化**

```bash
go fmt ./internal/coordinator/handle_ui_callbacks.go
```

---

## Task 6：验证与提交

- [ ] **Step 1: 全量编译**

```bash
cd wind_input && go build ./...
```

Expected: 无输出

- [ ] **Step 2: 运行现有测试**

```bash
cd wind_input && go test ./...
```

Expected: PASS（或与重构前相同的跳过/失败数）

- [ ] **Step 3: 人工测试核心场景**

安装并测试以下场景，确保行为与重构前一致：

1. **拼音分步上屏（InlinePreedit=ON）**：输入"zhongguo"，选"中国"（分步），继续选剩余，验证候选窗位置和嵌入文本正常。
2. **拼音分步上屏（InlinePreedit=OFF）**：相同操作，验证应用内无嵌入文本，仅候选窗显示。
3. **鼠标点击选词**：鼠标点选候选，验证文本正确上屏。
4. **组候选展开**（如有组候选方案）：验证展开行为正常。
5. **统计页面**：打开统计，验证鼠标选词也有统计记录（重构前鼠标路径无 recordCommit，此为 bug 修复）。

- [ ] **Step 4: 提交**

```bash
git add wind_input/internal/coordinator/handle_candidates.go \
        wind_input/internal/coordinator/handle_key_action.go \
        wind_input/internal/coordinator/handle_lifecycle.go \
        wind_input/internal/coordinator/handle_ui_callbacks.go

git commit -m "refactor(coordinator): 统一候选词选择逻辑至 doSelectCandidate

三套平行实现（键盘/内部/鼠标）合并为 doSelectCandidate，
InlinePreedit 检查收敛至 compositionUpdateResult 单一入口。
顺带修复 selectCandidateInternal 遗漏 inputHistory 记录、
handleCandidateSelect 遗漏 recordCommit 统计的问题。"
```

---

## 附录：重构前后对比

### 重构前

```
handleSpace()
    → selectCandidate()            ← 150 行实现，5 处 InlinePreedit 检查散落
handleSpaceInternal()
    → selectCandidateInternal()    ← 130 行重复实现，缺 inputHistory
handleCandidateSelect()（鼠标）    ← 140 行重复实现，缺 recordCommit / 组候选
```

### 重构后

```
handleSpace()         ┐
handleSpaceInternal() ┼→ doSelectCandidate()   ← 单一实现，全功能
handleCandidateSelect()┘      ↓
                       compositionUpdateResult() ← InlinePreedit 唯一检查点
                              ↓
                       pushKeyEventResult()      ← 鼠标路径 push 转换
```

新增任何选词入口只需调用 `doSelectCandidate`，配置逻辑自动生效，无需再改多处。
