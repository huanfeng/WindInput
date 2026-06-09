# 模式激活键优先级回落链 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把「模式激活键 vs 候选选择键」从互斥门禁改造成优先级回落链，并让所有触发键激活的模式（快捷输入/临时拼音/临时英文+未来模式）在正在输入时支持「顶码上屏当前高亮候选 + 进入模式」。

**Architecture:** 核心是「决策 / 执行分离」。纯函数 `decideBufferedTrigger` 裁决一个键在 buffer 非空时的归属（选候选 / 进模式 / overflow / 透传），返回纯数据结构，可完整单测；执行壳 `routeBufferedTriggerKey` 据此调用副作用。三个模式的触发判定收敛为「纯键匹配 helper + enabled 门禁 + setup 抽取」。

**Tech Stack:** Go；`internal/coordinator` 包；现有测试脚手架 `testhelper_test.go`（`newTestCoordinator` / `pressKey`）。

---

## 项目约定（务必遵守）

- **不主动 `git commit` / `git push`**：下列各 Task 的 commit 步骤是**逻辑分界**，便于回溯；实际提交由用户在真机验证后决定，或听用户指示。执行时若用户未明确授权，**只暂存改动、不执行 `git commit`**。
- 每次改完 Go 代码运行 `go fmt ./...`。
- 每次改完必须 `go build ./...` 确认可编译。
- 涉及 coordinator 目录对外结构变化时，按 `CLAUDE.md` 同步更新相关 `AGENTS.md`。
- 日志遵循隐私约定：INFO 级别不记录候选文本/编码，仅元数据。
- **测试 import**：各 Task 中测试代码块顶部的 `import (...)` 仅标注「该步需要哪些包」，执行时应**合并进 `mode_trigger_test.go` 单一 import 块**，勿写出重复 import 块；按需增删（Go 对未使用 import 报错）。

## 设计参考

- 设计文档：`docs/design/mode-trigger-priority-chain.md`（含优先级链 A~F、模式间优先级）。
- 现成范本：`internal/coordinator/handle_key_action.go:90-95`（五笔顶码上屏：`InsertText{HasNewComposition:true,NewComposition} + resetCompositionAnchorAfterCommit + armPendingFirstShow`）。

## 优先级回落链（实现目标，buffer 非空 / 有候选时）

| 优先级 | 角色 | 条件 | 决策 kind |
| --- | --- | --- | --- |
| A | 双拼韵母键 | 有 buffer + `isShuangpinFinalKey` | `actAlphaKey` |
| B | 二候选键 | `isSelectKey2` + 候选数 ≥ 2 | `actSelectCandidate(idx=pageStart+1)` |
| C | 三候选键 | `isSelectKey3` + 候选数 ≥ 3 | `actSelectCandidate(idx=pageStart+2)` |
| D | 模式激活键 | 按 `triggerModes()` 顺序首个 match 且 enabled | `actEnterMode(name,triggerKey,commitIdx)` |
| E | 二三候选键回落 | 是 B/C 键但候选不足、且未命中 D | `actOverflow(key)` |
| F | 透传 | 都不匹配 | `actNone` → 调用方继续走 switch（标点） |

- D 中若高亮候选 `IsGroup` 或 `len(Actions)>0` → 不命中 D，落到 E/F（与改动前行为一致）。
- 模式间顺序：快捷输入 → 临时拼音 →（未来模式）→ 临时英文。

## 文件结构

| 文件 | 责任 |
| --- | --- |
| `internal/coordinator/mode_trigger.go`（新建） | `triggerModeEntry` / `triggerModes()` / `matchTriggerKeyInList` / 决策类型 / `decideBufferedTrigger`（纯） / `routeBufferedTriggerKey`（执行） / `enterModeCommitting` |
| `internal/coordinator/mode_trigger_test.go`（新建） | 优先级回落链单测（防回归核心） |
| `internal/coordinator/handle_temp_pinyin.go`（改） | `matchTempPinyinTrigger` / `setupTempPinyinMode` / `enterTempPinyinMode` 重构 |
| `internal/coordinator/handle_quick_input.go`（改） | `matchQuickInputTrigger` / `setupQuickInputMode` / `enterQuickInputMode` 重构 |
| `internal/coordinator/handle_temp_english.go`（改） | `matchTempEnglishTrigger` / `setupTempEnglishMode` / `enterTempEnglishModeWithTrigger` 重构 |
| `internal/coordinator/handle_key_event.go`（改） | 调用点重排：buffer 非空走回落链；buffer 空走统一模式入口；`isSelectKey2/3` case 简化 |
| `internal/coordinator/testhelper_test.go`（改） | 新增 `withCandidates` / `withSelectKeyGroups` / `withQuickInputTriggers` 等 fixture option |

---

## Task 1: 公共触发键匹配 helper `matchTriggerKeyInList`

**Files:**
- Create: `internal/coordinator/mode_trigger.go`
- Test: `internal/coordinator/mode_trigger_test.go`

- [ ] **Step 1: 写失败测试**

`internal/coordinator/mode_trigger_test.go`：
```go
package coordinator

import (
	"testing"

	"github.com/huanfeng/wind_input/internal/ipc"
)

func TestMatchTriggerKeyInList(t *testing.T) {
	list := []string{"`", ";"}

	// 反引号：字面值匹配
	if got := matchTriggerKeyInList(list, "`", int(ipc.VK_OEM_3)); got != "`" {
		t.Errorf("grave by char: got %q, want `", got)
	}
	// 反引号：仅 VK 匹配（key 字段为空）
	if got := matchTriggerKeyInList(list, "", int(ipc.VK_OEM_3)); got != "`" {
		t.Errorf("grave by VK: got %q, want `", got)
	}
	// 分号
	if got := matchTriggerKeyInList(list, ";", int(ipc.VK_OEM_1)); got != ";" {
		t.Errorf("semicolon: got %q, want ;", got)
	}
	// 未配置的键
	if got := matchTriggerKeyInList(list, ".", int(ipc.VK_OEM_PERIOD)); got != "" {
		t.Errorf("period not in list: got %q, want empty", got)
	}
	// 空列表
	if got := matchTriggerKeyInList(nil, "`", int(ipc.VK_OEM_3)); got != "" {
		t.Errorf("nil list: got %q, want empty", got)
	}
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `go test ./internal/coordinator/ -run TestMatchTriggerKeyInList -v`
Expected: FAIL（`undefined: matchTriggerKeyInList`）

- [ ] **Step 3: 实现 helper**

`internal/coordinator/mode_trigger.go`：
```go
// mode_trigger.go — 触发键激活模式的统一优先级回落链。
// 设计见 docs/design/mode-trigger-priority-chain.md。
package coordinator

import (
	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/pkg/keys"
)

// triggerKeyVK 把规范化按键映射到 OEM 虚拟键码（触发键子集，纯标点键）。
var triggerKeyVK = map[keys.Key]uint32{
	keys.KeyGrave:     ipc.VK_OEM_3,
	keys.KeySemicolon: ipc.VK_OEM_1,
	keys.KeyQuote:     ipc.VK_OEM_7,
	keys.KeyComma:     ipc.VK_OEM_COMMA,
	keys.KeyPeriod:    ipc.VK_OEM_PERIOD,
	keys.KeySlash:     ipc.VK_OEM_2,
	keys.KeyBackslash: ipc.VK_OEM_5,
	keys.KeyLBracket:  ipc.VK_OEM_4,
	keys.KeyRBracket:  ipc.VK_OEM_6,
}

// matchTriggerKeyInList 判断 (key,keyCode) 是否匹配 triggerKeys 列表中的某个键，
// 返回匹配到的配置项字符串（空串=未匹配）。纯键匹配，不含任何状态门禁。
// 同时支持字面值（key 字符串）与 VK 码两种匹配，兼容 key 字段缺失的场景。
func matchTriggerKeyInList(triggerKeys []string, key string, keyCode int) string {
	if len(triggerKeys) == 0 {
		return ""
	}
	parsedKey, _ := keys.ParseKey(key)
	vk := uint32(keyCode)
	for _, tk := range triggerKeys {
		tkKey, _ := keys.ParseKey(tk)
		wantVK, ok := triggerKeyVK[tkKey]
		if !ok {
			continue
		}
		if parsedKey == tkKey || vk == wantVK {
			return tk
		}
	}
	return ""
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `go test ./internal/coordinator/ -run TestMatchTriggerKeyInList -v`
Expected: PASS

- [ ] **Step 5: go fmt + 暂存（不提交）**

```bash
go fmt ./internal/coordinator/
git add internal/coordinator/mode_trigger.go internal/coordinator/mode_trigger_test.go
# 按项目约定：暂不 git commit，等真机验证或用户指示
```

> 注：`triggerKeyVK` 未含 `z`——`z` 是字母键，不走本回落链（保留既有 `zHybridFallback` 路径）。

---

## Task 2: 各模式拆出「纯匹配」`matchXxxTrigger`

**Files:**
- Modify: `internal/coordinator/handle_quick_input.go`（`getQuickInputTriggerKey` 周边）
- Modify: `internal/coordinator/handle_temp_english.go`（`getTempEnglishTriggerKey` 周边）
- Modify: `internal/coordinator/handle_temp_pinyin.go`（`getTempPinyinTriggerKey` 周边）
- Test: `internal/coordinator/mode_trigger_test.go`

- [ ] **Step 1: 写失败测试（快捷输入纯匹配，不依赖引擎）**

追加到 `mode_trigger_test.go`：
```go
import (
	"github.com/huanfeng/wind_input/pkg/config"
)

func TestMatchQuickInputTrigger(t *testing.T) {
	cfg := &config.Config{}
	cfg.Input.QuickInput.TriggerKeys = []string{"`"}
	h := newTestCoordinator(t, withConfig(cfg))

	// enabled 且键匹配
	if got := h.matchQuickInputTrigger("`", int(ipc.VK_OEM_3)); got != "`" {
		t.Errorf("got %q, want `", got)
	}
	// 未配置触发键 → 不匹配
	empty := newTestCoordinator(t, withConfig(&config.Config{}))
	if got := empty.matchQuickInputTrigger("`", int(ipc.VK_OEM_3)); got != "" {
		t.Errorf("no trigger keys: got %q, want empty", got)
	}
}
```

- [ ] **Step 2: 运行确认失败**

Run: `go test ./internal/coordinator/ -run TestMatchQuickInputTrigger -v`
Expected: FAIL（`h.matchQuickInputTrigger undefined`）

- [ ] **Step 3: 实现三个 matchXxxTrigger（去掉状态门禁，保留 enabled）**

`handle_quick_input.go` 新增（保留旧 `getQuickInputTriggerKey` 暂不删，Task 6 删）：
```go
// matchQuickInputTrigger 纯触发键匹配 + enabled 门禁，不含 buffer/candidates 状态门禁。
// 状态优先级由 decideBufferedTrigger 统一裁决。
func (c *Coordinator) matchQuickInputTrigger(key string, keyCode int) string {
	if c.config == nil || len(c.config.Input.QuickInput.TriggerKeys) == 0 {
		return ""
	}
	return matchTriggerKeyInList(c.config.Input.QuickInput.TriggerKeys, key, keyCode)
}
```

`handle_temp_english.go` 新增：
```go
// matchTempEnglishTrigger 纯匹配 + enabled，不含状态门禁。
func (c *Coordinator) matchTempEnglishTrigger(key string, keyCode int) string {
	if c.config == nil || !c.config.Input.ShiftTempEnglish.Enabled {
		return ""
	}
	return matchTriggerKeyInList(c.config.Input.ShiftTempEnglish.TriggerKeys, key, keyCode)
}
```

`handle_temp_pinyin.go` 新增（含引擎门禁，去掉 `;`/`'` 的 `candidates==0` 内联门禁；`z` 仍由旧路径处理，不在此 helper 内）：
```go
// matchTempPinyinTrigger 纯匹配 + enabled（引擎类型 + 临时拼音开关），不含状态门禁。
// 不处理 z（z 走 handleAlphaKey/zHybridFallback 独立路径）。
func (c *Coordinator) matchTempPinyinTrigger(key string, keyCode int) string {
	if c.engineMgr == nil || !c.engineMgr.IsCurrentEngineType(schema.EngineTypeCodeTable) {
		return ""
	}
	if !c.engineMgr.IsTempPinyinEnabled() {
		return ""
	}
	if c.config == nil {
		return ""
	}
	// 过滤掉 z，仅匹配标点类触发键
	punctKeys := make([]string, 0, len(c.config.Input.TempPinyin.TriggerKeys))
	for _, tk := range c.config.Input.TempPinyin.TriggerKeys {
		if tk != "z" {
			punctKeys = append(punctKeys, tk)
		}
	}
	return matchTriggerKeyInList(punctKeys, key, keyCode)
}
```

- [ ] **Step 4: 运行确认通过**

Run: `go test ./internal/coordinator/ -run TestMatchQuickInputTrigger -v`
Expected: PASS

- [ ] **Step 5: 全量编译 + go fmt + 暂存**

```bash
go build ./...
go fmt ./internal/coordinator/
git add internal/coordinator/handle_quick_input.go internal/coordinator/handle_temp_english.go internal/coordinator/handle_temp_pinyin.go internal/coordinator/mode_trigger_test.go
```

---

## Task 3: 抽出 `setupXxxMode`，`enterXxxMode` 改薄封装

**Files:**
- Modify: `internal/coordinator/handle_temp_pinyin.go`
- Modify: `internal/coordinator/handle_quick_input.go`
- Modify: `internal/coordinator/handle_temp_english.go`

- [ ] **Step 1: 实现 setupTempPinyinMode + 重构 enterTempPinyinMode**

`handle_temp_pinyin.go`：把 `enterTempPinyinMode` 的状态设置抽成 `setupTempPinyinMode`，返回 `(prefix string, ok bool)`：
```go
// setupTempPinyinMode 设置临时拼音模式状态（不构造返回结果）。
// 返回 preedit 前缀字符与是否成功（拼音引擎加载失败 → false）。
func (c *Coordinator) setupTempPinyinMode(triggerKey string) (string, bool) {
	if c.engineMgr != nil {
		if err := c.engineMgr.EnsurePinyinLoaded(); err != nil {
			c.logger.Warn("Failed to load pinyin engine for temp pinyin", "error", err)
			return "", false
		}
		c.engineMgr.ActivateTempPinyin()
	}
	c.tempPinyinMode = true
	c.tempPinyinTriggerKey = triggerKey
	c.tempPinyinBuffer = ""
	c.tempPinyinCursorPos = 0
	c.tempPinyinCommitted = ""
	c.armPendingFirstShow()
	return c.tempPinyinPrefix(), true
}

// enterTempPinyinMode 空 buffer 进入临时拼音模式（薄封装）。
func (c *Coordinator) enterTempPinyinMode(triggerKey string) *bridge.KeyEventResult {
	prefix, ok := c.setupTempPinyinMode(triggerKey)
	if !ok {
		return nil
	}
	return c.modeCompositionResult(prefix, len(prefix))
}
```

- [ ] **Step 2: 实现 setupQuickInputMode + 重构 enterQuickInputMode**

`handle_quick_input.go`：
```go
// setupQuickInputMode 设置快捷输入模式状态（不构造返回结果）。返回 (prefix, true)。
func (c *Coordinator) setupQuickInputMode(triggerKey string) (string, bool) {
	c.quickInputMode = true
	c.quickInputTriggerKey = triggerKey
	c.quickInputBuffer = ""
	if c.config != nil && c.config.Input.QuickInput.ForceVertical {
		c.savedLayout = c.config.UI.CandidateLayout
		if c.uiManager != nil {
			c.uiManager.SetCandidateLayout(config.LayoutVertical)
		}
	}
	c.updateQuickInputCandidates()
	c.armPendingFirstShow()
	return c.quickInputPrefix(), true
}

// enterQuickInputMode 空 buffer 进入快捷输入模式（薄封装）。
func (c *Coordinator) enterQuickInputMode(triggerKey string) *bridge.KeyEventResult {
	prefix, _ := c.setupQuickInputMode(triggerKey)
	return c.modeCompositionResult(prefix, len(prefix))
}
```

- [ ] **Step 3: 实现 setupTempEnglishMode + 重构 enterTempEnglishModeWithTrigger**

`handle_temp_english.go`：阅读现有 `enterTempEnglishModeWithTrigger`（`:153`）的状态设置，照搬抽成 `setupTempEnglishMode(triggerKey) (string, bool)`，返回临时英文 preedit 前缀；`enterTempEnglishModeWithTrigger` 改为调用 setup 后 `return c.modeCompositionResult(prefix, len(prefix))`。

> 实现者注意：阅读 `handle_temp_english.go:153-210` 确认其 preedit 前缀函数名（如 `tempEnglishPrefix`）与状态字段；若该模式进入有特殊副作用（如布局），一并搬进 setup。

- [ ] **Step 4: 编译 + 现有测试回归**

Run: `go build ./... && go test ./internal/coordinator/ -v`
Expected: 全部 PASS（含既有 z/临时拼音/路由测试，证明重构无回归）

- [ ] **Step 5: go fmt + 暂存**

```bash
go fmt ./internal/coordinator/
git add internal/coordinator/handle_temp_pinyin.go internal/coordinator/handle_quick_input.go internal/coordinator/handle_temp_english.go
```

---

## Task 4: 纯决策函数 `decideBufferedTrigger`（防回归核心）

**Files:**
- Modify: `internal/coordinator/mode_trigger.go`
- Modify: `internal/coordinator/testhelper_test.go`（新增 fixture option）
- Test: `internal/coordinator/mode_trigger_test.go`

- [ ] **Step 1: 新增测试 fixture option**

`testhelper_test.go` 追加：
```go
import "github.com/huanfeng/wind_input/internal/candidate"
import "github.com/huanfeng/wind_input/pkg/keys"

// withCandidates 注入候选 + inputBuffer（模拟"正在输入有候选"）。
func withCandidates(buffer string, cands ...candidate.Candidate) testOption {
	return func(c *Coordinator) {
		c.inputBuffer = buffer
		c.inputCursorPos = len(buffer)
		c.candidates = cands
		c.currentPage = 1
		c.selectedIndex = 0
	}
}

// withSelectKeyGroups 配置二三候选选择键分组（如 PairSemicolonQuote → ; 二候选 / ' 三候选）。
func withSelectKeyGroups(groups ...keys.PairGroup) testOption {
	return func(c *Coordinator) {
		if c.config == nil {
			c.config = &config.Config{}
		}
		c.config.Input.SelectKeyGroups = groups
	}
}

// withQuickInputTriggers 配置快捷输入触发键（enabled 仅依赖此项，不触达引擎）。
func withQuickInputTriggers(keysList ...string) testOption {
	return func(c *Coordinator) {
		if c.config == nil {
			c.config = &config.Config{}
		}
		c.config.Input.QuickInput.TriggerKeys = keysList
	}
}
```

- [ ] **Step 2: 写决策链失败测试（用快捷输入作模式载体，纯 config 无引擎）**

`mode_trigger_test.go` 追加：
```go
import "github.com/huanfeng/wind_input/internal/candidate"

func cand(text string) candidate.Candidate { return candidate.Candidate{Text: text} }

// ; 同时是二候选键(PairSemicolonQuote 第一键) 和 快捷输入触发键。
func newSemicolonDualCoordinator(t *testing.T, buffer string, cands ...candidate.Candidate) *testCoordinator {
	return newTestCoordinator(t,
		withSelectKeyGroups(keys.PairSemicolonQuote),
		withQuickInputTriggers(";"),
		withCandidates(buffer, cands...),
	)
}

func TestDecide_SelectKey2_WinsWhenEnoughCandidates(t *testing.T) {
	// 候选 ≥ 2：; 选第 2 候选（B 优先于 D）
	h := newSemicolonDualCoordinator(t, "ab", cand("啊"), cand("吧"))
	d := h.decideBufferedTrigger(";", int(ipc.VK_OEM_1))
	if d.kind != actSelectCandidate || d.candidateIdx != 1 {
		t.Fatalf("got kind=%v idx=%d, want actSelectCandidate idx=1", d.kind, d.candidateIdx)
	}
}

func TestDecide_FallbackToMode_WhenCandidatesInsufficient(t *testing.T) {
	// 只有 1 个候选：; 二候选无效 → 回落快捷输入（D），顶码上屏高亮候选(idx 0)
	h := newSemicolonDualCoordinator(t, "ab", cand("啊"))
	d := h.decideBufferedTrigger(";", int(ipc.VK_OEM_1))
	if d.kind != actEnterMode || d.modeName != "quick_input" || d.commitIdx != 0 {
		t.Fatalf("got %+v, want actEnterMode quick_input commitIdx=0", d)
	}
}

func TestDecide_PureModeKey_WithCandidates(t *testing.T) {
	// ` 不是选择键，纯模式键：有候选 → 进模式，顶码上屏高亮候选
	h := newTestCoordinator(t,
		withQuickInputTriggers("`"),
		withCandidates("ab", cand("啊"), cand("吧")),
	)
	d := h.decideBufferedTrigger("`", int(ipc.VK_OEM_3))
	if d.kind != actEnterMode || d.commitIdx != 0 {
		t.Fatalf("got %+v, want actEnterMode commitIdx=0", d)
	}
}

func TestDecide_EmptyCandidates_DiscardAndEnter(t *testing.T) {
	// buffer 非空但无候选（空码）：commitIdx=-1
	h := newTestCoordinator(t,
		withQuickInputTriggers("`"),
		withCandidates("zzz"), // 无候选
	)
	d := h.decideBufferedTrigger("`", int(ipc.VK_OEM_3))
	if d.kind != actEnterMode || d.commitIdx != -1 {
		t.Fatalf("got %+v, want actEnterMode commitIdx=-1", d)
	}
}

func TestDecide_GroupCandidate_FallsThroughToOverflow(t *testing.T) {
	// 高亮是组候选 + ; 是二候选键候选不足 → 不进模式，回落 overflow（与改动前一致）
	h := newSemicolonDualCoordinator(t, "ab", candidate.Candidate{Text: "组", IsGroup: true})
	d := h.decideBufferedTrigger(";", int(ipc.VK_OEM_1))
	if d.kind != actOverflow {
		t.Fatalf("got %+v, want actOverflow", d)
	}
}

func TestDecide_GroupCandidate_PureModeKey_FallsThroughToNone(t *testing.T) {
	// 高亮是组候选 + ` 纯模式键 → 不进模式，actNone（调用方走标点）
	h := newTestCoordinator(t,
		withQuickInputTriggers("`"),
		withCandidates("ab", candidate.Candidate{Text: "组", IsGroup: true}),
	)
	d := h.decideBufferedTrigger("`", int(ipc.VK_OEM_3))
	if d.kind != actNone {
		t.Fatalf("got %+v, want actNone", d)
	}
}

func TestDecide_NonRoleKey_None(t *testing.T) {
	// 句号既非选择键也非模式键 → actNone
	h := newTestCoordinator(t,
		withQuickInputTriggers("`"),
		withCandidates("ab", cand("啊"), cand("吧")),
	)
	d := h.decideBufferedTrigger(".", int(ipc.VK_OEM_PERIOD))
	if d.kind != actNone {
		t.Fatalf("got %+v, want actNone", d)
	}
}
```

- [ ] **Step 3: 运行确认失败**

Run: `go test ./internal/coordinator/ -run TestDecide -v`
Expected: FAIL（`decideBufferedTrigger` / `actEnterMode` 等 undefined）

- [ ] **Step 4: 实现决策类型 + triggerModes + decideBufferedTrigger**

`mode_trigger.go` 追加：
```go
// triggerActionKind 是 buffer 非空时一个触发键的归属裁决。
type triggerActionKind int

const (
	actNone           triggerActionKind = iota // 未处理，调用方继续（标点等）
	actAlphaKey                                // 双拼韵母键送引擎
	actSelectCandidate                         // 选候选
	actEnterMode                               // 顶码上屏 + 进模式
	actOverflow                                // 二三候选键候选不足回落
)

// bufferedTriggerDecision 是纯数据裁决结果（无副作用，便于单测）。
type bufferedTriggerDecision struct {
	kind         triggerActionKind
	candidateIdx int    // actSelectCandidate
	modeName     string // actEnterMode
	triggerKey   string // actEnterMode
	commitIdx    int    // actEnterMode：顶码上屏候选索引，-1=空码
	overflowKey  string // actOverflow
}

// triggerModeEntry 描述一个触发键激活的模式（轻量模式表项）。
type triggerModeEntry struct {
	name  string
	match func(key string, keyCode int) string         // 含 enabled；空=不匹配
	setup func(triggerKey string) (string, bool)       // 设置模式状态，返回 (prefix, ok)
}

// triggerModes 按优先级返回模式表。
// 顺序：快捷输入 > 临时拼音 >（未来模式插此）> 临时英文。
// 详见 docs/design/mode-trigger-priority-chain.md。
func (c *Coordinator) triggerModes() []triggerModeEntry {
	return []triggerModeEntry{
		{name: "quick_input", match: c.matchQuickInputTrigger, setup: c.setupQuickInputMode},
		{name: "temp_pinyin", match: c.matchTempPinyinTrigger, setup: c.setupTempPinyinMode},
		// ★ 未来模式（生僻字 / 符号码表）插入此处
		{name: "temp_english", match: c.matchTempEnglishTrigger, setup: c.setupTempEnglishMode},
	}
}

// decideBufferedTrigger 裁决 buffer 非空 / 有候选时一个 !hasShift 键的归属。
// 纯函数：只读状态，无副作用。优先级 A~F 见设计文档。
func (c *Coordinator) decideBufferedTrigger(key string, keyCode int) bufferedTriggerDecision {
	pageStart := (c.currentPage - 1) * c.candidatesPerPage

	// A. 双拼韵母键优先送引擎
	if len(c.inputBuffer) > 0 && c.isShuangpinFinalKey(key) {
		return bufferedTriggerDecision{kind: actAlphaKey}
	}

	isSel2 := c.isSelectKey2(key, keyCode)
	isSel3 := c.isSelectKey3(key, keyCode)

	// B. 二候选键 + 候选 ≥ 2
	if isSel2 {
		idx := pageStart + 1
		if idx < len(c.candidates) && idx-pageStart < c.candidatesPerPage {
			return bufferedTriggerDecision{kind: actSelectCandidate, candidateIdx: idx}
		}
	}
	// C. 三候选键 + 候选 ≥ 3
	if isSel3 {
		idx := pageStart + 2
		if idx < len(c.candidates) && idx-pageStart < c.candidatesPerPage {
			return bufferedTriggerDecision{kind: actSelectCandidate, candidateIdx: idx}
		}
	}

	// D. 模式激活键（按优先级遍历）
	for _, m := range c.triggerModes() {
		tk := m.match(key, keyCode)
		if tk == "" {
			continue
		}
		commitIdx := -1
		if len(c.candidates) > 0 {
			hi := pageStart + c.selectedIndex
			if hi >= len(c.candidates) {
				hi = 0
			}
			cnd := c.candidates[hi]
			if cnd.IsGroup || len(cnd.Actions) > 0 {
				// 高亮是组/命令候选：不进模式，回落 E/F（与改动前一致）
				break
			}
			commitIdx = hi
		}
		return bufferedTriggerDecision{
			kind: actEnterMode, modeName: m.name, triggerKey: tk, commitIdx: commitIdx,
		}
	}

	// E. 二三候选键候选不足回落
	if isSel2 || isSel3 {
		return bufferedTriggerDecision{kind: actOverflow, overflowKey: key}
	}

	// F. 透传
	return bufferedTriggerDecision{kind: actNone}
}
```

- [ ] **Step 5: 运行确认通过**

Run: `go test ./internal/coordinator/ -run TestDecide -v`
Expected: 全部 PASS

- [ ] **Step 6: 编译 + go fmt + 暂存**

```bash
go build ./...
go fmt ./internal/coordinator/
git add internal/coordinator/mode_trigger.go internal/coordinator/mode_trigger_test.go internal/coordinator/testhelper_test.go
```

---

## Task 5: 执行层 `enterModeCommitting` + `routeBufferedTriggerKey`

**Files:**
- Modify: `internal/coordinator/mode_trigger.go`

- [ ] **Step 1: 实现 enterModeCommitting**

`mode_trigger.go` 追加：
```go
// findTriggerMode 按 name 取模式表项。
func (c *Coordinator) findTriggerMode(name string) (triggerModeEntry, bool) {
	for _, m := range c.triggerModes() {
		if m.name == name {
			return m, true
		}
	}
	return triggerModeEntry{}, false
}

// enterModeCommitting 顶码上屏当前高亮候选（commitIdx>=0）或丢弃空码（-1），随后进入模式。
// 用 InsertText{HasNewComposition} 把"上屏文本"与"开启模式 preedit"合并为一个原子结果。
// 返回 nil 表示放弃（候选非普通文本 / 模式 setup 失败），调用方应回落后续处理。
func (c *Coordinator) enterModeCommitting(name, triggerKey string, commitIdx int) *bridge.KeyEventResult {
	entry, ok := c.findTriggerMode(name)
	if !ok {
		return nil
	}

	var finalText string
	if commitIdx >= 0 {
		res := c.doSelectCandidate(commitIdx)
		// 仅"完全上屏"(InsertText) 才继续进模式；组候选二级展开/cmdbar 等非 InsertText → 放弃
		if res == nil || res.Type != bridge.ResponseTypeInsertText {
			return nil
		}
		finalText = res.Text
	} else {
		c.clearState()
	}

	prefix, setupOK := entry.setup(triggerKey)
	if !setupOK {
		return nil
	}

	if finalText != "" {
		c.resetCompositionAnchorAfterCommit()
		newComp := ""
		if c.isInlinePreedit() {
			newComp = prefix
		}
		return &bridge.KeyEventResult{
			Type:              bridge.ResponseTypeInsertText,
			Text:              finalText,
			HasNewComposition: true,
			NewComposition:    newComp,
		}
	}
	return c.modeCompositionResult(prefix, len(prefix))
}

// routeBufferedTriggerKey 在 buffer 非空 / 有候选时按优先级回落链处理一个 !hasShift 键。
// 返回 nil 表示本链未处理，调用方继续后续 switch（标点等）。
func (c *Coordinator) routeBufferedTriggerKey(key string, data *bridge.KeyEventData) *bridge.KeyEventResult {
	d := c.decideBufferedTrigger(key, data.KeyCode)
	switch d.kind {
	case actAlphaKey:
		return c.handleAlphaKey(key)
	case actSelectCandidate:
		return c.selectCandidate(d.candidateIdx)
	case actEnterMode:
		return c.enterModeCommitting(d.modeName, d.triggerKey, d.commitIdx)
	case actOverflow:
		return c.handleOverflowSelectKey(d.overflowKey)
	default:
		return nil
	}
}
```

- [ ] **Step 2: 编译**

Run: `go build ./...`
Expected: 成功（无 unused 错误）

- [ ] **Step 3: 运行包内全部测试**

Run: `go test ./internal/coordinator/ -v`
Expected: 全部 PASS

- [ ] **Step 4: go fmt + 暂存**

```bash
go fmt ./internal/coordinator/
git add internal/coordinator/mode_trigger.go
```

---

## Task 6: 接入 `handle_key_event.go` 调用点 + 清理旧入口

**Files:**
- Modify: `internal/coordinator/handle_key_event.go:497-510`（模式激活块）
- Modify: `internal/coordinator/handle_key_event.go:712-761`（isSelectKey2/3 case）
- Modify: 删除旧 `getQuickInputTriggerKey` / `getTempEnglishTriggerKey` / `getTempPinyinTriggerKey`（若已无引用）

- [ ] **Step 1: 替换模式激活块（`:497-510`）**

把原三段 `if triggerKey := c.getXxxTriggerKey(...)` 替换为：
```go
	// 正在输入（有 buffer / 有候选）时：统一优先级回落链
	// （二三候选 > 模式激活 > overflow > 标点）。详见
	// docs/design/mode-trigger-priority-chain.md。
	if !hasShift && c.chineseMode && (len(c.inputBuffer) > 0 || len(c.candidates) > 0) {
		if r := c.routeBufferedTriggerKey(key, &data); r != nil {
			return r
		}
	}

	// buffer 为空且无候选：保留原三段 getXxxTriggerKey 调用，仅按新优先级
	// 顺序重排（快捷输入 > 临时拼音 > 临时英文）。
	// ★ 必须保留旧 getXxxTriggerKey（不可改用 matchXxx 遍历）：getTempPinyinTriggerKey
	//   内含 z 键的首次触发临时拼音逻辑（buffer 空 + 按 z + z 无码表前缀），而
	//   matchTempPinyinTrigger 已排除 z（z 走 zHybridFallback 独立回退路径，
	//   后者只处理"已以 z 开头"的回退，不处理首次触发）。改用 matchXxx 遍历会
	//   丢失 z 首次触发 → 回归。
	if triggerKey := c.getQuickInputTriggerKey(key, data.KeyCode); !hasShift && triggerKey != "" {
		return c.enterQuickInputMode(triggerKey)
	}
	if triggerKey := c.getTempPinyinTriggerKey(key, data.KeyCode); !hasShift && triggerKey != "" {
		return c.enterTempPinyinMode(triggerKey)
	}
	if triggerKey := c.getTempEnglishTriggerKey(key, data.KeyCode); !hasShift && triggerKey != "" {
		return c.enterTempEnglishModeWithTrigger(triggerKey)
	}
```

> 注意保留原 `hasShift` 与 `c.chineseMode` 判定语义。原 `:512` 起的 Shift+字母处理等其它分支不动。
> getXxxTriggerKey 内部已有 `len(inputBuffer)>0 || len(candidates)>0 → return ""` 门禁，buffer 非空时这三段不会触发，放在回落链之后安全。

- [ ] **Step 2: 简化 isSelectKey2 case（`:712`）**

buffer 非空时的二候选选择已由回落链接管，case 只保留「无 buffer → 标点」：
```go
	case !hasShift && c.isSelectKey2(key, data.KeyCode):
		// buffer 非空时的二候选/overflow 已由 routeBufferedTriggerKey 接管，
		// 这里只处理无输入缓冲时的标点回退。
		if len(c.inputBuffer) == 0 && len(key) == 1 && c.isPunctuation(rune(key[0])) {
			return c.handlePunctuation(rune(key[0]), prevDigitState, data.PrevChar)
		}
		return nil
```

- [ ] **Step 3: 简化 isSelectKey3 case（`:739`）**

```go
	case !hasShift && c.isSelectKey3(key, data.KeyCode):
		// buffer 非空时的三候选/overflow 已由 routeBufferedTriggerKey 接管，
		// 这里只处理无输入缓冲时的标点回退。
		if len(c.inputBuffer) == 0 && len(key) == 1 && c.isPunctuation(rune(key[0])) {
			return c.handlePunctuation(rune(key[0]), prevDigitState, data.PrevChar)
		}
		return nil
```

> `isPinyinSeparator` case（`:736`）保持不变，位于二三候选之间。

- [ ] **Step 4: 保留旧 getXxxTriggerKey（不删除）**

旧 `getQuickInputTriggerKey` / `getTempPinyinTriggerKey` / `getTempEnglishTriggerKey` **保留**——buffer-空入口仍调用它们（见 Step 1 的 z 键说明）。本计划不删除这三个函数。
Run: `grep -rn "getQuickInputTriggerKey\|getTempEnglishTriggerKey\|getTempPinyinTriggerKey" internal/` 确认三者仍被 Step 1 的 buffer-空入口引用即可。

- [ ] **Step 5: 编译 + 全量测试**

Run: `go build ./... && go test ./internal/coordinator/ -v`
Expected: 全部 PASS

- [ ] **Step 6: go fmt + 暂存**

```bash
go fmt ./internal/coordinator/
git add internal/coordinator/handle_key_event.go internal/coordinator/handle_quick_input.go internal/coordinator/handle_temp_english.go internal/coordinator/handle_temp_pinyin.go
```

---

## Task 7: 集成回归测试 + 全量验证 + 文档

**Files:**
- Modify: `internal/coordinator/mode_trigger_test.go`
- Modify: 相关 `AGENTS.md`（若 coordinator 对外结构变化）

- [ ] **Step 1: 模式间优先级测试**

`mode_trigger_test.go` 追加（同键绑定多模式时命中优先级最高者）：
```go
func TestDecide_ModePriority_QuickInputBeatsTempPinyin(t *testing.T) {
	// ; 同时配给快捷输入；快捷输入优先级高于临时拼音，应命中 quick_input。
	// （临时拼音 enabled 依赖码表引擎，此处未挂引擎 → 仅快捷输入 enabled，
	//  仍能验证遍历顺序不会先命中后者。）
	h := newTestCoordinator(t,
		withQuickInputTriggers(";"),
		withSelectKeyGroups(keys.PairSemicolonQuote),
		withCandidates("ab", cand("啊")), // 1 个候选 → 二候选无效 → 进模式
	)
	d := h.decideBufferedTrigger(";", int(ipc.VK_OEM_1))
	if d.kind != actEnterMode || d.modeName != "quick_input" {
		t.Fatalf("got %+v, want actEnterMode quick_input", d)
	}
}
```

- [ ] **Step 2: 临时拼音引擎门禁回归测试**

复用 `withZHybridSchema`（码表引擎 + 临时拼音启用）验证临时拼音在码表引擎下可匹配、非码表不可：
```go
func TestMatchTempPinyin_EngineGate(t *testing.T) {
	// 码表引擎 + 临时拼音启用 + 配 ` 触发键 → 匹配
	h := newTestCoordinator(t, withZHybridSchema(false))
	h.config.Input.TempPinyin.TriggerKeys = append(h.config.Input.TempPinyin.TriggerKeys, "`")
	if got := h.matchTempPinyinTrigger("`", int(ipc.VK_OEM_3)); got != "`" {
		t.Errorf("codetable engine: got %q, want `", got)
	}
	// 无引擎 → 不匹配（引擎门禁）
	bare := newTestCoordinator(t)
	bare.config.Input.TempPinyin.TriggerKeys = []string{"`"}
	if got := bare.matchTempPinyinTrigger("`", int(ipc.VK_OEM_3)); got != "" {
		t.Errorf("no engine: got %q, want empty", got)
	}
}
```

- [ ] **Step 3: z 键不受影响回归测试**

```go
func TestMatchTempPinyin_ExcludesZ(t *testing.T) {
	// z 配为临时拼音触发键，但 matchTempPinyinTrigger 不应匹配 z（z 走独立路径）
	h := newTestCoordinator(t, withZHybridSchema(false)) // 已含 "z" 触发键
	if got := h.matchTempPinyinTrigger("z", int('Z')); got != "" {
		t.Errorf("z should be excluded from punct trigger match: got %q", got)
	}
}
```

- [ ] **Step 4: 运行新增测试**

Run: `go test ./internal/coordinator/ -run "TestDecide_ModePriority|TestMatchTempPinyin" -v`
Expected: 全部 PASS

- [ ] **Step 5: 全仓回归**

```bash
go build ./...
go test ./...
go fmt ./...
```
Expected: 编译成功；`./internal/coordinator/` 及全仓测试全部 PASS（确认无回归）。

- [ ] **Step 6: 更新 AGENTS.md（若需要）**

Run: `pwsh scripts/lint_agents_md.ps1`
- 若 coordinator 目录新增对外结构（`mode_trigger.go` 的导出符号——本计划新符号均为非导出方法/类型，通常无需）→ 按 `docs/AGENTS-TEMPLATE.md` 更新 `internal/coordinator/AGENTS.md`。
- 设计文档 `docs/design/mode-trigger-priority-chain.md` 已记录模式间优先级，无需重复。

- [ ] **Step 7: 暂存（等真机验证）**

```bash
git add internal/coordinator/ docs/
# 真机验证清单见下方"真机验证（不可自动化）"。验证通过后由用户决定 git commit。
```

---

## 真机验证（不可自动化，TSF 交互）

单元测试覆盖优先级裁决（防回归核心），但「上屏 + 重启 composition」的真实行为依赖 TSF，需真机手测：

1. 码表方案配 `` ` `` 为临时拼音引导符：输入有候选 → 按 `` ` `` → 高亮候选上屏 + 进临时拼音、`` ` `` 不上屏。
2. 移动高亮后按 `` ` `` → 上屏的是高亮候选。
3. 空码 → 按 `` ` `` → 编码丢弃直接进临时拼音。
4. `;` 配二候选键 + 临时拼音触发键：候选 ≥ 2 选第 2 候选；仅 1 候选 → 顶码上屏 + 进临时拼音。
5. 快捷输入 / 临时英文 同样验证「顶码上屏 + 进模式」。
6. 高亮为组候选 / cmdbar 命令 → 按模式键不进模式、不触发命令。
7. inline 与非 inline preedit 两种配置：上屏后候选窗定位正确、触发键字符不嵌入宿主应用（重点测 WPS/Excel/EverEdit）。
8. 顶码上屏候选被正确学习（自动造词）、计入输入历史。
9. 非码表引擎（混输/全拼）→ 临时拼音不触发；z 键混输回退行为不变。

## Self-Review 覆盖核对

- 优先级链 A~F → Task 4 决策函数 + 测试全覆盖。
- 模式间优先级（快捷>临时拼音>临时英文） → `triggerModes()` 顺序 + Task 7 Step 1 测试。
- 顶码上屏复用 doSelectCandidate + HasNewComposition → Task 5 `enterModeCommitting`。
- 空码丢弃 → Task 4 `commitIdx=-1` 分支 + 测试。
- group/cmdbar 回落 → Task 4/5 + 测试。
- 二三候选 switch 简化避免双重处理 → Task 6 Step 2/3。
- z 不受影响 → Task 2 `matchTempPinyinTrigger` 过滤 z + Task 7 Step 3 测试。
- 不依赖 punct_commit → 全程未引用 punct_commit。
- 防回归 → Task 3 Step 4 / Task 5 Step 3 / Task 6 Step 5 / Task 7 Step 5 多次全量 `go test`。
