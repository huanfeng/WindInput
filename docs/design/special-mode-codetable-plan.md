# 引导键特殊模式（自定义码表）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: 用 superpowers:subagent-driven-development（推荐）或
> superpowers:executing-plans 逐任务实施。步骤用 checkbox（`- [ ]`）跟踪。

**Goal:** 新增一种由引导键进入、配置驱动的特殊模式，加载自定义 Rime 码表，支持 prefix-free /
定长 / 手动三档自动上屏，候选完整支持命令直通车与变量、数组。

**Architecture:** 复用 `dict.CodeTable` + Rime 加载链作存储；新增 `specialMode` 状态机（仿
`quickInputMode`）；接入现有 `triggerModes()` 优先级链；把 `$AA`/`$SS` 数组展开抽成 phrase 与
special-table 共享的设施。设计见 `docs/design/special-mode-codetable.md`。

**Tech Stack:** Go；`internal/dict`（CodeTable / dictcache / value_expand）、
`internal/coordinator`（模式状态机 / triggerModes）、`pkg/config`。

**实施顺序与依赖**：Task 1→2 独立基础；Task 3（码表加载）依赖 1；Task 4（判定纯函数）独立；
Task 5（状态机）依赖 2/3/4；Task 6（接入触发链）依赖 5；Task 7（数组统一，最高风险）独立但放最后，
带 phrase 回归。每个 Task 完成后程序可编译、测试通过。

**通用约束（每个 Task 都遵守）**
- 改完 Go 代码必须 `gofmt -w <改动文件>` + `go build ./...`。
- INFO 日志只记元数据（实例 id、码表条目数、编码长度），**禁止**记码表文本 / 候选内容 / 用户输入。
- 提交信息**不带** Co-Authored-By / Constraint / Confidence 等 AI 附加信息；功能未真机测试前由用户决定是否提交（本计划的 commit 步骤是建议节点，实际提交听用户）。

---

### Task 1: 配置结构 `SpecialModeConfig`

**Files:**
- Modify: `wind_input/pkg/config/config.go`（`InputConfig` 结构体增字段 + 默认值块 + 新类型）
- Test: `wind_input/pkg/config/special_mode_test.go`（新建）

**背景**：`InputConfig` 已有 `QuickInput`、`TempPinyin` 等子配置（见 config.go:313 一带）。
本任务加 `SpecialModes []SpecialModeConfig` 与校验。

- [ ] **Step 1: 写失败测试**

新建 `wind_input/pkg/config/special_mode_test.go`：

```go
package config

import "testing"

func TestSpecialModeConfig_Validate(t *testing.T) {
	cases := []struct {
		name string
		cfg  SpecialModeConfig
		ok   bool
	}{
		{"ok_prefix_free", SpecialModeConfig{ID: "sym", TriggerKeys: []string{"grave"}, Table: "a.dict.yaml", AutoCommit: "prefix_free"}, true},
		{"ok_fixed_len", SpecialModeConfig{ID: "rare", TriggerKeys: []string{"semicolon"}, Table: "b.dict.yaml", AutoCommit: "fixed_length", FixedLength: 4}, true},
		{"ok_manual", SpecialModeConfig{ID: "m", TriggerKeys: []string{"quote"}, Table: "c.dict.yaml", AutoCommit: "manual"}, true},
		{"empty_id", SpecialModeConfig{ID: "", TriggerKeys: []string{"grave"}, Table: "a", AutoCommit: "manual"}, false},
		{"empty_triggers", SpecialModeConfig{ID: "x", TriggerKeys: nil, Table: "a", AutoCommit: "manual"}, false},
		{"empty_table", SpecialModeConfig{ID: "x", TriggerKeys: []string{"grave"}, Table: "", AutoCommit: "manual"}, false},
		{"bad_strategy", SpecialModeConfig{ID: "x", TriggerKeys: []string{"grave"}, Table: "a", AutoCommit: "weird"}, false},
		{"fixed_len_zero", SpecialModeConfig{ID: "x", TriggerKeys: []string{"grave"}, Table: "a", AutoCommit: "fixed_length", FixedLength: 0}, false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := tc.cfg.Validate()
			if tc.ok && err != nil {
				t.Fatalf("want ok, got err: %v", err)
			}
			if !tc.ok && err == nil {
				t.Fatalf("want err, got nil")
			}
		})
	}
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd wind_input && go test ./pkg/config/ -run TestSpecialModeConfig_Validate -v`
Expected: 编译失败 `undefined: SpecialModeConfig`。

- [ ] **Step 3: 实现类型与校验**

在 `config.go` 中 `QuickInputConfig` 定义附近新增：

```go
// 特殊模式自动上屏策略
const (
	SpecialAutoCommitPrefixFree  = "prefix_free"  // 唯一候选且无更长前缀
	SpecialAutoCommitFixedLength = "fixed_length" // 达固定码长且唯一候选
	SpecialAutoCommitManual      = "manual"       // 永远手动选
)

// SpecialModeConfig 引导键特殊模式（自定义码表）单实例配置
type SpecialModeConfig struct {
	ID            string   `yaml:"id" json:"id"`
	Name          string   `yaml:"name" json:"name"`                 // 模式徽标显示名
	TriggerKeys   []string `yaml:"trigger_keys" json:"trigger_keys"` // 引导键
	Table         string   `yaml:"table" json:"table"`               // 码表文件，相对 schemas 目录
	AutoCommit    string   `yaml:"auto_commit" json:"auto_commit"`   // prefix_free|fixed_length|manual
	FixedLength   int      `yaml:"fixed_length,omitempty" json:"fixed_length,omitempty"`
	ForceVertical bool     `yaml:"force_vertical,omitempty" json:"force_vertical,omitempty"`
	AccentColor   string   `yaml:"accent_color,omitempty" json:"accent_color,omitempty"`
	// —— 预留字段，MVP 不实现 ——
	CodeCharset string   `yaml:"code_charset,omitempty" json:"code_charset,omitempty"`
	Schemes     []string `yaml:"schemes,omitempty" json:"schemes,omitempty"`
	Engines     []string `yaml:"engines,omitempty" json:"engines,omitempty"`
}

// Validate 校验单实例配置（不校验文件是否存在，那由 registry 在加载时做）。
func (s SpecialModeConfig) Validate() error {
	if s.ID == "" {
		return fmt.Errorf("special mode: id 不能为空")
	}
	if len(s.TriggerKeys) == 0 {
		return fmt.Errorf("special mode %q: trigger_keys 不能为空", s.ID)
	}
	if s.Table == "" {
		return fmt.Errorf("special mode %q: table 不能为空", s.ID)
	}
	switch s.AutoCommit {
	case SpecialAutoCommitPrefixFree, SpecialAutoCommitManual:
	case SpecialAutoCommitFixedLength:
		if s.FixedLength <= 0 {
			return fmt.Errorf("special mode %q: auto_commit=fixed_length 时 fixed_length 必须 > 0", s.ID)
		}
	default:
		return fmt.Errorf("special mode %q: 未知 auto_commit=%q", s.ID, s.AutoCommit)
	}
	return nil
}
```

在 `InputConfig` 结构体（含 `QuickInput QuickInputConfig` 那块，config.go:313 附近）追加字段：

```go
	SpecialModes []SpecialModeConfig `yaml:"special_modes,omitempty" json:"special_modes,omitempty"`
```

> `fmt` 包 config.go 已 import；若未 import 则补上。默认配置块（config.go:547 `QuickInput:` 一带）
> **无需**为 `SpecialModes` 加默认值（空列表 = 关闭，是合理默认）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cd wind_input && go test ./pkg/config/ -run TestSpecialModeConfig_Validate -v`
Expected: PASS（8 子用例全绿）。

- [ ] **Step 5: 编译 + 提交（建议节点）**

```bash
cd wind_input && gofmt -w pkg/config/config.go pkg/config/special_mode_test.go && go build ./...
git add pkg/config/config.go pkg/config/special_mode_test.go
git commit -m "feat(special-mode): 新增 SpecialModeConfig 配置结构与校验"
```

---

### Task 2: 上屏来源 `SourceSpecialMode`

**Files:**
- Modify: `wind_input/internal/store/stats.go:20-31`

**背景**：`recordCommit(text, codeLen, candidatePos, source)` 需要一个新来源标识区分特殊模式上屏。
现有常量见 stats.go:20（iota 块，末尾是 `commitSourceCount`）。

- [ ] **Step 1: 加常量**

在 `SourceTSFDirect` 之后、`commitSourceCount` 之前插入一行：

```go
	SourceTSFDirect                       // TSF 层直接输入 (英文模式)
	SourceSpecialMode                     // 引导键特殊模式（自定义码表）
	commitSourceCount                     // 来源总数（内部使用）
```

> `commitSourceCount` 用于内部数组定长，插在它前面自动顺延，无需改其它处。若 `stats.go` 内有
> 依赖具体来源数值的序列化（检查 `CandPosDist`/统计落盘），确认新增枚举值不破坏已落盘数据的反序列化
> （iota 追加在尾部、不插中间即安全）。

- [ ] **Step 2: 编译**

Run: `cd wind_input && go build ./...`
Expected: 通过，无未定义引用。

- [ ] **Step 3: 提交（建议节点）**

```bash
cd wind_input && git add internal/store/stats.go
git commit -m "feat(special-mode): 新增 SourceSpecialMode 上屏来源"
```

---

### Task 3: 特殊码表独立加载器 + 实例注册表

**Files:**
- Create: `wind_input/internal/coordinator/special_mode_registry.go`
- Create: `wind_input/internal/coordinator/special_mode_registry_test.go`
- Create（测试夹具）: `wind_input/internal/coordinator/testdata/special_symbols.dict.yaml`

**背景**：要把每个实例的码表独立加载成 `*dict.CodeTable`（不进主 CompositeDict）。加载范式镜像
`internal/schema/factory.go:521 loadCodetable`：用 `dictcache.RimeCodetableSourcePaths` +
`dictcache.NeedsRegenerate` + `dictcache.CachePath(cacheKey)` +
`dictcache.ConvertRimeCodetableToWdb(srcPath, wdbCachePath, logger)`，最后 `CodeTable.LoadBinary(wdb)`。
与 factory 不同：目标是裸 `*dict.CodeTable`（用 `dict.NewCodeTable()` + `LoadBinary`），不是
`codetable.Engine`。

**码表懒加载 + 缓存**：registry 持有实例列表（配置顺序），`match` 返回首个命中的 enabled 实例 id，
`ensureLoaded` 首次激活才转换 + 加载并缓存到实例上。

- [ ] **Step 1: 写夹具码表**

新建 `wind_input/internal/coordinator/testdata/special_symbols.dict.yaml`（Rime 格式，code-first 列序）：

```yaml
---
name: special_symbols
sort: by_order
columns:
  - code
  - text
...
jt	→
jt	←
xh	①
arrow	⇧
```

> 说明：`jt` 两个候选（→/←），`xh` 单候选，`arrow` 单候选且是 `arr...` 唯一前缀。

- [ ] **Step 2: 写失败测试**

新建 `wind_input/internal/coordinator/special_mode_registry_test.go`：

```go
package coordinator

import (
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/pkg/config"
)

func TestSpecialModeRegistry_MatchAndLoad(t *testing.T) {
	dir, _ := filepath.Abs("testdata")
	reg := newSpecialModeRegistry([]config.SpecialModeConfig{
		{ID: "sym", Name: "快符", TriggerKeys: []string{"grave"}, Table: "special_symbols.dict.yaml", AutoCommit: "prefix_free"},
	}, dir, testLogger())

	// 触发键匹配：` (grave / VK_OEM_3)
	if id := reg.match("`", 0xC0); id != "sym" {
		t.Fatalf("match grave want sym, got %q", id)
	}
	if id := reg.match("a", 0x41); id != "" {
		t.Fatalf("match 'a' want empty, got %q", id)
	}

	// 懒加载码表 + 查询 + HasLongerCode
	inst := reg.get("sym")
	if inst == nil {
		t.Fatal("get(sym) nil")
	}
	tbl, err := reg.ensureLoaded(inst)
	if err != nil {
		t.Fatalf("ensureLoaded: %v", err)
	}
	if got := tbl.Lookup("jt"); len(got) != 2 {
		t.Fatalf("Lookup(jt) want 2 cands, got %d", len(got))
	}
	if !tbl.HasLongerCode("arr") {
		t.Fatalf("HasLongerCode(arr) want true")
	}
	if tbl.HasLongerCode("arrow") {
		t.Fatalf("HasLongerCode(arrow) want false")
	}
}
```

> `testLogger()` 若 testhelper 未提供，新增一个返回 `slog.New(slog.NewTextHandler(io.Discard, nil))`
> 的小 helper（放 `special_mode_registry_test.go` 内即可）。

- [ ] **Step 3: 跑测试确认失败**

Run: `cd wind_input && go test ./internal/coordinator/ -run TestSpecialModeRegistry_MatchAndLoad -v`
Expected: 编译失败 `undefined: newSpecialModeRegistry`。

- [ ] **Step 4: 实现 registry**

新建 `wind_input/internal/coordinator/special_mode_registry.go`：

```go
// special_mode_registry.go — 引导键特殊模式实例注册表 + 码表懒加载。
// 设计见 docs/design/special-mode-codetable.md。
package coordinator

import (
	"fmt"
	"log/slog"
	"path/filepath"
	"sync"

	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/dict/dictcache"
	"github.com/huanfeng/wind_input/pkg/config"
)

// specialModeInstance 单实例运行态：配置 + 懒加载的码表。
type specialModeInstance struct {
	cfg     config.SpecialModeConfig
	table   *dict.CodeTable // 懒加载，nil=未加载
	loadErr error           // 上次加载错误（避免反复重试时刷屏）
}

// specialModeRegistry 按配置顺序持有实例，提供触发键匹配 + 懒加载。
type specialModeRegistry struct {
	mu         sync.Mutex
	instances  []*specialModeInstance // 配置顺序
	schemasDir string                 // 码表文件相对此目录解析
	logger     *slog.Logger
}

// newSpecialModeRegistry 校验配置、去重 id/触发键，构造注册表。
// 无效实例跳过 + WARN（不阻断其它实例）。
func newSpecialModeRegistry(cfgs []config.SpecialModeConfig, schemasDir string, logger *slog.Logger) *specialModeRegistry {
	r := &specialModeRegistry{schemasDir: schemasDir, logger: logger}
	seenID := map[string]bool{}
	seenKey := map[string]string{} // triggerKey -> 抢占它的 instance id
	for _, c := range cfgs {
		if err := c.Validate(); err != nil {
			logger.Warn("special mode 配置无效，跳过", "err", err.Error())
			continue
		}
		if seenID[c.ID] {
			logger.Warn("special mode id 重复，跳过", "id", c.ID)
			continue
		}
		// 触发键去重：靠前者优先，靠后者该键失效（仍保留实例，只是被抢的键不计）
		for _, k := range c.TriggerKeys {
			if owner, ok := seenKey[k]; ok {
				logger.Warn("special mode 触发键被占用", "key", k, "owner", owner, "skipped", c.ID)
			} else {
				seenKey[k] = c.ID
			}
		}
		seenID[c.ID] = true
		r.instances = append(r.instances, &specialModeInstance{cfg: c})
	}
	return r
}

// match 返回首个命中 (key,keyCode) 的实例 id（空串=未命中）。
// 复用 mode_trigger.go 的 matchTriggerKeyInList（纯键匹配，含 VK 兜底）。
func (r *specialModeRegistry) match(key string, keyCode int) string {
	for _, inst := range r.instances {
		if matchTriggerKeyInList(inst.cfg.TriggerKeys, key, keyCode) != "" {
			return inst.cfg.ID
		}
	}
	return ""
}

// get 按 id 取实例。
func (r *specialModeRegistry) get(id string) *specialModeInstance {
	for _, inst := range r.instances {
		if inst.cfg.ID == id {
			return inst
		}
	}
	return nil
}

// ensureLoaded 懒加载实例码表（转 wdb + LoadBinary），结果缓存到实例上。
func (r *specialModeRegistry) ensureLoaded(inst *specialModeInstance) (*dict.CodeTable, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if inst.table != nil {
		return inst.table, nil
	}
	srcPath := filepath.Join(r.schemasDir, inst.cfg.Table)
	cacheKey := "special-" + inst.cfg.ID
	wdbPath := dictcache.CachePath(cacheKey)
	srcPaths := dictcache.RimeCodetableSourcePaths(srcPath)
	if len(srcPaths) == 0 || dictcache.NeedsRegenerate(srcPaths, wdbPath) {
		if err := dictcache.ConvertRimeCodetableToWdb(srcPath, wdbPath, r.logger); err != nil {
			inst.loadErr = fmt.Errorf("转换特殊码表失败 %s: %w", inst.cfg.ID, err)
			return nil, inst.loadErr
		}
	}
	ct := dict.NewCodeTable()
	if err := ct.LoadBinary(wdbPath); err != nil {
		inst.loadErr = fmt.Errorf("加载特殊码表 wdb 失败 %s: %w", inst.cfg.ID, err)
		return nil, inst.loadErr
	}
	inst.table = ct
	r.logger.Info("特殊码表已加载", "id", inst.cfg.ID, "entries", ct.EntryCount())
	return ct, nil
}
```

> **验证 `dictcache.ConvertRimeCodetableToWdb` 的可变参数签名**（factory.go:558 调用形如
> `ConvertRimeCodetableToWdb(srcPath, wdbCachePath, logger, normalizer)`，normalizer 可省）。
> `dictcache.CachePath` / `NeedsRegenerate` / `RimeCodetableSourcePaths` 均为现有导出函数。

- [ ] **Step 5: 跑测试确认通过**

Run: `cd wind_input && go test ./internal/coordinator/ -run TestSpecialModeRegistry_MatchAndLoad -v`
Expected: PASS。

- [ ] **Step 6: 编译 + 提交（建议节点）**

```bash
cd wind_input && gofmt -w internal/coordinator/special_mode_registry.go internal/coordinator/special_mode_registry_test.go && go build ./...
git add internal/coordinator/special_mode_registry.go internal/coordinator/special_mode_registry_test.go internal/coordinator/testdata/special_symbols.dict.yaml
git commit -m "feat(special-mode): 实例注册表与 Rime 码表懒加载"
```

---

### Task 4: 自动上屏判定纯函数

**Files:**
- Create: `wind_input/internal/coordinator/special_mode_decide.go`
- Create: `wind_input/internal/coordinator/special_mode_decide_test.go`

**背景**：三档自动上屏判定抽成无副作用纯函数，便于穷举单测。`prefix_free` 用 `HasLongerCode` 的结果。

- [ ] **Step 1: 写失败测试**

新建 `wind_input/internal/coordinator/special_mode_decide_test.go`：

```go
package coordinator

import (
	"testing"

	"github.com/huanfeng/wind_input/pkg/config"
)

func TestDecideSpecialAutoCommit(t *testing.T) {
	type in struct {
		strategy    string
		fixedLength int
		bufLen      int
		candCount   int
		hasLonger   bool
	}
	cases := []struct {
		name string
		in   in
		want bool
	}{
		{"prefix_free 唯一无后续→上屏", in{config.SpecialAutoCommitPrefixFree, 0, 2, 1, false}, true},
		{"prefix_free 唯一有后续→等", in{config.SpecialAutoCommitPrefixFree, 0, 2, 1, true}, false},
		{"prefix_free 多候选→等", in{config.SpecialAutoCommitPrefixFree, 0, 2, 3, false}, false},
		{"prefix_free 零候选→等", in{config.SpecialAutoCommitPrefixFree, 0, 2, 0, false}, false},
		{"fixed_length 达长且唯一→上屏", in{config.SpecialAutoCommitFixedLength, 4, 4, 1, false}, true},
		{"fixed_length 达长多候选→等", in{config.SpecialAutoCommitFixedLength, 4, 4, 2, false}, false},
		{"fixed_length 未达长→等", in{config.SpecialAutoCommitFixedLength, 4, 3, 1, false}, false},
		{"manual 永远不上屏", in{config.SpecialAutoCommitManual, 0, 9, 1, false}, false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := decideSpecialAutoCommit(tc.in.strategy, tc.in.fixedLength, tc.in.bufLen, tc.in.candCount, tc.in.hasLonger)
			if got != tc.want {
				t.Fatalf("got %v want %v", got, tc.want)
			}
		})
	}
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd wind_input && go test ./internal/coordinator/ -run TestDecideSpecialAutoCommit -v`
Expected: 编译失败 `undefined: decideSpecialAutoCommit`。

- [ ] **Step 3: 实现纯函数**

新建 `wind_input/internal/coordinator/special_mode_decide.go`：

```go
// special_mode_decide.go — 特殊模式自动上屏判定（纯函数，无副作用，便于单测）。
package coordinator

import "github.com/huanfeng/wind_input/pkg/config"

// decideSpecialAutoCommit 判定当前是否应自动上屏。
//   strategy:    实例 auto_commit
//   fixedLength: 实例 fixed_length（仅 fixed_length 档用）
//   bufLen:      当前编码长度
//   candCount:   当前直接候选数（展开前的精确码候选条数）
//   hasLonger:   码表中是否存在以当前编码为前缀的更长编码
func decideSpecialAutoCommit(strategy string, fixedLength, bufLen, candCount int, hasLonger bool) bool {
	switch strategy {
	case config.SpecialAutoCommitPrefixFree:
		return candCount == 1 && !hasLonger
	case config.SpecialAutoCommitFixedLength:
		return fixedLength > 0 && bufLen >= fixedLength && candCount == 1
	default: // manual 及未知
		return false
	}
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cd wind_input && go test ./internal/coordinator/ -run TestDecideSpecialAutoCommit -v`
Expected: PASS（8 子用例全绿）。

- [ ] **Step 5: 编译 + 提交（建议节点）**

```bash
cd wind_input && gofmt -w internal/coordinator/special_mode_decide.go internal/coordinator/special_mode_decide_test.go && go build ./...
git add internal/coordinator/special_mode_decide.go internal/coordinator/special_mode_decide_test.go
git commit -m "feat(special-mode): 三档自动上屏判定纯函数"
```

---

### Task 5: `specialMode` 状态机

**Files:**
- Create: `wind_input/internal/coordinator/handle_special_mode.go`
- Modify: `wind_input/internal/coordinator/coordinator.go`（新增状态字段 + `clearState` 清理 +
  `hasActiveInput` 纳入 + registry 字段与构造）
- Test: `wind_input/internal/coordinator/handle_special_mode_test.go`（新建）

**背景**：仿 `handle_quick_input.go`（quickInputState 见 coordinator.go:169；进入逻辑见
`setupQuickInputMode` handle_quick_input.go:153；退出清理见 `exitQuickInputMode` :482）。状态机
**不进主引擎**，自持 buffer，查实例码表算候选 + 自动上屏。命令 / 变量候选用 coordinator 现有
`applyValueExpansion`（handle_candidates.go:331，处理 `$CC`/`$X`）；数组 `$AA`/`$SS` 在 Task 7
接入统一展开后才生效，本任务先只接 `applyValueExpansion`（文本 + `$CC` + `$X`）。

**关键复用**：`modeCompositionResult`（pinyin_mode_shared.go:536）、`armPendingFirstShow`
（handle_lifecycle.go:33）、`modeAccentColor`（coordinator.go:1046）、`isInlinePreedit`
（handle_candidates.go:823）、`recordCommit`（stats.go:40）。

#### 5a. coordinator 结构与构造

- [ ] **Step 1: 加状态字段与 registry**

在 coordinator.go 的 `quickInputState`（:169-180）之后新增：

```go
// specialModeState 引导键特殊模式（自定义码表）状态
type specialModeState struct {
	specialMode        bool                   // 是否处于特殊模式
	specialActiveID    string                 // 当前激活实例 id
	specialTriggerKey  string                 // 当前触发键
	specialBuffer      string                 // 编码缓冲（不含触发符）
	specialSavedLayout config.CandidateLayout // 进入前布局（force_vertical 时恢复用）
}
```

把 `specialModeState` 嵌入 `Coordinator` 结构体（与 `quickInputState` 嵌入处并列），并加一个
registry 字段：

```go
	specialModeReg *specialModeRegistry
```

在 `NewCoordinator`（构造收尾、`installCmdbarPhraseHook()` 之后，coordinator.go:772 一带）装配
registry：

```go
	// 特殊模式注册表（码表懒加载）。schemasDir 复用方案发现使用的 schemas 目录。
	c.specialModeReg = newSpecialModeRegistry(c.config.Input.SpecialModes, c.schemasDir(), c.logger)
```

> `c.schemasDir()`：若 Coordinator 已有 exeDir/dataDir 字段，返回 `filepath.Join(exeDir, "schemas")`
> （与 `internal/schema` 的 `DiscoverSchemas(exeDir, dataDir)` 同源，见 schema/AGENTS.md:13）。
> 若没有现成入口，新增一个私有方法读取与 schema loader 相同的路径来源；**禁止**硬编码绝对路径。

- [ ] **Step 2: clearState 清理 + hasActiveInput 纳入**

`clearState`（coordinator.go:984 一带，quickInput 清理见 :1005-1027）追加特殊模式清理（恢复布局须在
重置标志前）：

```go
	if c.specialMode {
		if c.specialSavedLayout != "" && c.uiManager != nil {
			c.uiManager.SetCandidateLayout(c.specialSavedLayout)
		}
		if c.uiManager != nil {
			c.uiManager.SetModeLabel("")
			c.uiManager.SetModeAccentColor(nil)
		}
	}
	c.specialMode = false
	c.specialActiveID = ""
	c.specialTriggerKey = ""
	c.specialBuffer = ""
	c.specialSavedLayout = ""
```

`hasActiveInput`（coordinator.go:930-934，含 `c.quickInputMode`）追加 `|| c.specialMode`。

#### 5b. 进入 / setup

- [ ] **Step 3: 写进入逻辑**（仿 `setupQuickInputMode`）

在新建 `handle_special_mode.go` 中实现触发键匹配 + setup（供 Task 6 的 triggerModes 调用）：

```go
// handle_special_mode.go — 引导键特殊模式（自定义码表）状态机。
// 设计见 docs/design/special-mode-codetable.md。
package coordinator

import (
	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/internal/store"
	"github.com/huanfeng/wind_input/pkg/config"
)

// matchSpecialTrigger 纯触发键匹配（含 enabled），返回触发键字符串（空=不匹配）。
// id 为空时遍历所有实例（buffer 空入口用）；非空时只匹配指定实例（triggerModes 项用）。
func (c *Coordinator) matchSpecialTrigger(id, key string, keyCode int) string {
	if c.specialModeReg == nil {
		return ""
	}
	inst := c.specialModeReg.get(id)
	if inst == nil {
		return ""
	}
	return matchTriggerKeyInList(inst.cfg.TriggerKeys, key, keyCode)
}

// setupSpecialMode 设置特殊模式状态（不构造返回结果）。返回 (prefix, ok)。
// prefix = 触发键字符（与 quickInputPrefix 同源），用于 preedit 前缀。
func (c *Coordinator) setupSpecialMode(id, triggerKey string) (string, bool) {
	inst := c.specialModeReg.get(id)
	if inst == nil {
		return "", false
	}
	// 懒加载码表；失败放弃进入（回落标点）。
	if _, err := c.specialModeReg.ensureLoaded(inst); err != nil {
		c.logger.Warn("特殊模式码表加载失败，放弃进入", "id", id, "err", err.Error())
		return "", false
	}

	c.specialMode = true
	c.specialActiveID = id
	c.specialTriggerKey = triggerKey
	c.specialBuffer = ""

	if inst.cfg.ForceVertical {
		c.specialSavedLayout = c.config.UI.CandidateLayout
		if c.uiManager != nil {
			c.uiManager.SetCandidateLayout(config.LayoutVertical)
		}
	}

	c.updateSpecialCandidates() // buffer 空 → 候选空
	c.armPendingFirstShow()
	return c.specialPrefix(), true
}

// specialPrefix 当前触发键对应的字面字符（复用 quickInputPrefix 的键→字符映射）。
func (c *Coordinator) specialPrefix() string {
	return triggerKeyToChar(c.specialTriggerKey)
}
```

> **抽公共映射**：`quickInputPrefix`（handle_quick_input.go:128）里「键名→字符」的 switch 抽成包级
> `triggerKeyToChar(triggerKey string) string`，`quickInputPrefix` 改为调它（等价重构）。`specialPrefix`
> 复用。放 `mode_trigger.go` 或新文件均可。

#### 5c. 候选计算 + 自动上屏

- [ ] **Step 4: 写候选计算**

```go
// updateSpecialCandidates 用当前 specialBuffer 查实例码表，算候选 + 自动上屏判定结果。
// 返回 autoCommit=true 时调用方应立即上屏首候选并退出。
func (c *Coordinator) updateSpecialCandidates() (autoCommit bool) {
	inst := c.specialModeReg.get(c.specialActiveID)
	if inst == nil || inst.table == nil {
		c.candidates = nil
		c.totalPages = 1
		return false
	}
	buf := c.specialBuffer
	if buf == "" {
		c.candidates = nil
		c.totalPages = 1
		c.currentPage = 1
		c.selectedIndex = 0
		return false
	}

	raw := inst.table.Lookup(buf) // []candidate.Candidate（精确码）
	hasLonger := inst.table.HasLongerCode(buf)

	// 转 ui.Candidate + 统一展开（$CC/$X；$AA/$SS 待 Task 7）。
	c.candidates = c.buildSpecialUICandidates(raw)

	// 自动上屏判定基于「精确码直接候选条数」（展开前）。
	auto := decideSpecialAutoCommit(inst.cfg.AutoCommit, inst.cfg.FixedLength, len(buf), len(raw), hasLonger)

	c.refreshEffectivePerPage()
	total := len(c.candidates)
	c.totalPages = (total + c.candidatesPerPage - 1) / c.candidatesPerPage
	if c.totalPages < 1 {
		c.totalPages = 1
	}
	if c.currentPage > c.totalPages {
		c.currentPage = c.totalPages
	}
	if c.currentPage < 1 {
		c.currentPage = 1
	}
	return auto
}
```

> `buildSpecialUICandidates(raw []candidate.Candidate) []ui.Candidate`：把码表候选转 `ui.Candidate`
> （设置 `Text`/`Comment`/`Index`），对每条调 `applyValueExpansion`（需先把 `ui.Candidate` 与
> `candidate.Candidate` 字段对接——参考 `updateCandidatesEx` handle_candidates.go:500 一带把 engine
> 候选转 UI 候选并 `applyValueExpansion(&cand)` 的写法，镜像该转换）。命令候选（Actions 非空）保留
> Actions 以走 cmdbar 执行通路。

#### 5d. 按键处理 + 退出

- [ ] **Step 5: 写按键处理 + 退出**（仿 `handleQuickInputKey` :190 / `exitQuickInputMode` :482）

实现 `handleSpecialModeKey(key string, data *bridge.KeyEventData) *bridge.KeyEventResult`，控制键映射：

| 键 | 行为 |
| --- | --- |
| `ipc.VK_SPACE` | 选当前高亮候选；空 buffer 上屏 `specialPrefix()` 字面量 |
| `ipc.VK_RETURN` | buffer 非空上屏 buffer 原文；空 buffer 上屏 `specialPrefix()` |
| `ipc.VK_BACK` | 删末字符；空 buffer 再退格 → `exitSpecialMode(false,"")` |
| `ipc.VK_ESCAPE` | `exitSpecialMode(false,"")` |
| 数字 `1-9` | 选当前页第 (n-1) 候选 → 上屏退出 |
| 字母 `a-z`（小写归一） | 追加到 `specialBuffer`，`updateSpecialCandidates()`；若返回 autoCommit → 选首候选上屏退出；否则 `showSpecialUI()` + `modeCompositionResult(prefix+buffer, len)` |
| 翻页 / 高亮键 | 复用 `isQuickInputPageUpKey`/`isHighlightUpKey` 等同款判断，仅 `showSpecialUI()` |
| 其它 | `ResponseTypeConsumed` |

选候选与上屏统一走：

```go
// selectSpecialCandidate 选指定全局索引候选并退出（命令候选走 Actions 通路）。
func (c *Coordinator) selectSpecialCandidate(index int) *bridge.KeyEventResult {
	if index < 0 || index >= len(c.candidates) {
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}
	cand := c.candidates[index]
	if len(cand.Actions) > 0 {
		// 命令候选：复用 doSelectCandidate 的 cmdbar 执行通路语义。
		// 退出模式后交给现有动作执行（参考 handle_candidates 中 Actions 非空的处理）。
		return c.commitSpecialCommand(cand)
	}
	text := cand.Text
	if c.fullWidth {
		text = transform.ToFullWidth(text)
	}
	if c.inputHistory != nil {
		c.inputHistory.Record(text, "", "", 0)
	}
	return c.exitSpecialMode(true, text)
}

// exitSpecialMode 退出并按需上屏（仿 exitQuickInputMode）。
func (c *Coordinator) exitSpecialMode(commit bool, text string) *bridge.KeyEventResult {
	if c.specialSavedLayout != "" && c.uiManager != nil {
		c.uiManager.SetCandidateLayout(c.specialSavedLayout)
		c.specialSavedLayout = ""
	}
	if c.uiManager != nil {
		c.uiManager.SetModeLabel("")
		c.uiManager.SetModeAccentColor(nil)
	}
	c.specialMode = false
	c.specialActiveID = ""
	c.specialTriggerKey = ""
	c.specialBuffer = ""
	c.candidates = nil
	c.currentPage = 1
	c.totalPages = 1
	c.selectedIndex = 0
	c.hideUI()
	if commit && len(text) > 0 {
		c.recordCommit(text, 0, -1, store.SourceSpecialMode)
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeInsertText, Text: text}
	}
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
}
```

> `commitSpecialCommand(cand ui.Candidate)`：镜像 `doSelectCandidate` 对 `Actions` 非空候选的处理
> （handle_candidates.go / AGENTS.md:54 描述：返回 `ResponseTypeClearComposition` 并在 goroutine 内
> 顺序执行动作，错误只记 WARN 元数据）。先 `exitSpecialMode(false,"")` 清理状态，再触发动作执行。
> 直接复用既有的动作执行入口，不要复制动作执行实现。

> `showSpecialUI()`：镜像 `showQuickInputUI`（handle_quick_input.go:527），差异仅：`SetModeLabel`
> 用 `inst.cfg.Name`、`SetModeAccentColor` 用 `c.modeAccentColor("special:"+id)`（或实例 accent_color）、
> 嵌入编码下空 buffer 把 `preedit=""` 触发徽标提示条（复用 handle_quick_input.go:577-581 的写法）。

- [ ] **Step 6: 写状态机测试**

新建 `handle_special_mode_test.go`，覆盖（用 Task 3 夹具码表 + mock uiManager/engineMgr，参考
`testhelper_test.go` 现有 mock 构造）：
- 进入后打 `arrow`：prefix_free 档，`HasLongerCode("arrow")==false` 且唯一候选 → autoCommit=true。
- 打 `ar`：`HasLongerCode("ar")==true` → 不自动上屏，显示候选。
- 打 `jt`：2 候选 → 不自动上屏；按数字 `1` 选「→」上屏并退出（`specialMode==false`）。
- 空 buffer 退格 → 退出模式。
- Esc → 退出，返回 `ResponseTypeClearComposition`。

> 纯判定部分（autoCommit）已被 Task 4 覆盖；本测聚焦状态流转。若 mock 成本高，至少覆盖
> 「进入→打码→autoCommit 路径」「数字选候选→退出」「Esc/退格退出」三条。

- [ ] **Step 7: 跑测试 + 编译 + 提交（建议节点）**

```bash
cd wind_input && go test ./internal/coordinator/ -run TestSpecial -v
gofmt -w internal/coordinator/handle_special_mode.go internal/coordinator/coordinator.go internal/coordinator/handle_special_mode_test.go && go build ./...
git add internal/coordinator/handle_special_mode.go internal/coordinator/handle_special_mode_test.go internal/coordinator/coordinator.go
git commit -m "feat(special-mode): specialMode 状态机（进入/输入/选择/退出/自动上屏）"
```

---

### Task 6: 接入触发键优先级链

**Files:**
- Modify: `wind_input/internal/coordinator/mode_trigger.go`（`triggerModes()` 动态构建）
- Modify: `wind_input/internal/coordinator/handle_key_event.go`（buffer 空入口 + 已在模式内分发）
- Test: `wind_input/internal/coordinator/special_mode_trigger_test.go`（新建）

**背景**：`triggerModes()`（mode_trigger.go:77）现返回固定 3 项。改为动态：在 temp_pinyin 之后、
temp_english 之前插入 N 个 special 实例项。每项 `match`/`setup` 闭包绑定实例 id。
`enterModeCommitting`（:160）无需改动——它通过 `entry.setup(triggerKey)` 已统一处理顶码上屏 + 嵌入
编码。还需：① 已在特殊模式内时，`handle_key_event.go` 主入口分发到 `handleSpecialModeKey`；② buffer
空时按引导键直接进特殊模式（参考现有「buffer 空走旧 getXxxTriggerKey」路径）。

- [ ] **Step 1: 写失败测试**

新建 `special_mode_trigger_test.go`，验证 `triggerModes()` 顺序与命中：

```go
package coordinator

import (
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/pkg/config"
)

func TestTriggerModes_SpecialInserted(t *testing.T) {
	dir, _ := filepath.Abs("testdata")
	c := newTestCoordinator(t) // testhelper 现有构造；若无则参考 testhelper_test.go
	c.config.Input.SpecialModes = []config.SpecialModeConfig{
		{ID: "sym", Name: "快符", TriggerKeys: []string{"grave"}, Table: "special_symbols.dict.yaml", AutoCommit: "prefix_free"},
	}
	c.specialModeReg = newSpecialModeRegistry(c.config.Input.SpecialModes, dir, c.logger)

	modes := c.triggerModes()
	// 顺序：quick_input, temp_pinyin, special:sym, temp_english
	var names []string
	for _, m := range modes {
		names = append(names, m.name)
	}
	wantOrderContains := []string{"quick_input", "temp_pinyin", "special:sym", "temp_english"}
	// 断言 special:sym 在 temp_pinyin 之后、temp_english 之前
	idxPinyin, idxSpecial, idxEng := indexOf(names, "temp_pinyin"), indexOf(names, "special:sym"), indexOf(names, "temp_english")
	if !(idxPinyin < idxSpecial && idxSpecial < idxEng) {
		t.Fatalf("order wrong: %v (want %v)", names, wantOrderContains)
	}
}

func indexOf(ss []string, s string) int {
	for i, v := range ss {
		if v == s {
			return i
		}
	}
	return -1
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd wind_input && go test ./internal/coordinator/ -run TestTriggerModes_SpecialInserted -v`
Expected: FAIL（special:sym 不在列表里，`idxSpecial == -1`）。

- [ ] **Step 3: 改 `triggerModes()` 动态构建**

```go
func (c *Coordinator) triggerModes() []triggerModeEntry {
	modes := []triggerModeEntry{
		{name: "quick_input", match: c.matchQuickInputTrigger, setup: c.setupQuickInputMode},
		{name: "temp_pinyin", match: c.matchTempPinyinTrigger, setup: c.setupTempPinyinMode},
	}
	// 特殊模式实例（配置顺序）插入临时拼音之后、临时英文之前。
	if c.specialModeReg != nil {
		for _, inst := range c.specialModeReg.instances {
			id := inst.cfg.ID
			modes = append(modes, triggerModeEntry{
				name:  "special:" + id,
				match: func(key string, keyCode int) string { return c.matchSpecialTrigger(id, key, keyCode) },
				setup: func(triggerKey string) (string, bool) { return c.setupSpecialMode(id, triggerKey) },
			})
		}
	}
	modes = append(modes, triggerModeEntry{name: "temp_english", match: c.matchTempEnglishTrigger, setup: c.setupTempEnglishMode})
	return modes
}
```

> 闭包捕获 `id`（循环内已用局部 `id := inst.cfg.ID`，Go 1.22+ 循环变量每轮新实例亦安全）。

- [ ] **Step 4: 主入口分发 + buffer 空入口**

`handle_key_event.go`：在已分发 `quickInputMode → handleQuickInputKey` 的同款位置，追加
`if c.specialMode { return c.handleSpecialModeKey(key, &data) }`（置于与其它模式分发并列处）。

buffer 空时按引导键进入：在「buffer 空走旧 getXxxTriggerKey」段落，追加 special 入口（仅当未处于任何
模式、buffer 空、`!hasShift`）：

```go
	if id := c.specialModeReg.match(key, data.KeyCode); id != "" {
		if tk := c.matchSpecialTrigger(id, key, data.KeyCode); tk != "" {
			if prefix, ok := c.setupSpecialMode(id, tk); ok {
				return c.modeCompositionResult(prefix, len(prefix))
			}
		}
	}
```

> 放置点须在「正在输入时走 `routeBufferedTriggerKey`」之后的 buffer-空分支，避免与优先级链重复。
> 参考现有 quick_input / temp_pinyin 的 buffer-空入口位置，保持一致。

- [ ] **Step 5: 跑测试 + 集成回归**

Run:
```bash
cd wind_input && go test ./internal/coordinator/ -run 'TestTriggerModes_SpecialInserted|TestSpecial' -v
go test ./internal/coordinator/...
```
Expected: 新测 PASS；现有 mode_trigger / 候选 / 临时模式测试**不回归**。

- [ ] **Step 6: 编译 + 提交（建议节点）**

```bash
cd wind_input && gofmt -w internal/coordinator/mode_trigger.go internal/coordinator/handle_key_event.go internal/coordinator/special_mode_trigger_test.go && go build ./...
git add internal/coordinator/mode_trigger.go internal/coordinator/handle_key_event.go internal/coordinator/special_mode_trigger_test.go
git commit -m "feat(special-mode): 接入触发键优先级链与按键分发"
```

---

### Task 7: 统一数组展开（`$AA`/`$SS`）+ phrase 等价重构【最高风险，最后做】

**Files:**
- Modify: `wind_input/internal/dict/value_expand.go`（扩展为可产出多候选 + 持数组 hook）
- Modify: `wind_input/internal/dict/phrase.go`（`SearchCommand` 数组路径改调共享入口）
- Modify: `wind_input/internal/coordinator/handle_special_mode.go`（`buildSpecialUICandidates` 接数组展开）
- Test: `wind_input/internal/dict/value_expand_array_test.go`（新建）
- Test: 现有 `wind_input/internal/dict/phrase_ss_test.go` / `phrase_multigroup_test.go` 必须全绿（回归）

**背景**：现状 `ValueExpander.Expand`（value_expand.go:65）只产出**单**候选，处理 `$CC`/`$X`；
`$AA`/`$SS` 的**多候选**展开耦合在 `phrase.go` 的 `SearchCommand`（依赖 `cmdbarArrayHook` +
`aa_marker.go`/`ss_marker.go`）。本任务把数组展开抽成 phrase 与 special-table 共享的入口。

**实施策略（提取，不重写）**：
1. **先读懂现状**：精读 `phrase.go` 中 `SearchCommand` 处理 `$AA`（字符组）与 `$SS`（字符串数组）
   的两段（`expandSSGroupSingle` / 字符组成员展开），以及 `aa_marker.go`/`ss_marker.go` 的 marker 解析。
   理清输入（raw value + 两个 hook）与输出（多条候选，含 GroupCode/GroupName/Actions）。
2. **定义共享入口**（value_expand.go）：

```go
// ExpandToCandidates 把一个 raw value 展开成一条或多条候选。
//   - 纯文本 / $X：1 条
//   - $CC：1 条（含 Actions）
//   - $AA/$SS：N 条（字符 / 元素级，含 GroupCode/GroupName）
// ArrayHook 为 nil 时 $AA/$SS 退化为字面量 1 条。
func (ve *ValueExpander) ExpandToCandidates(code, value string) []candidate.Candidate
```

   `ValueExpander` 增字段 `ArrayHook CmdbarArrayHook`（已是 dict 内类型，见 phrase.go:76）。
3. **phrase.go 改调**：`SearchCommand` 的数组分支改为调 `ExpandToCandidates`（**行为必须逐字段等价**：
   GroupCode/GroupName/GroupTemplate/RawText 派生 id、weight、Actions 全部保持）。这是回归重点。
4. **special-table 接入**：`buildSpecialUICandidates` 改调 `ExpandToCandidates`（用 `c.cmdbarValueExpander`，
   并确保其 `ArrayHook` 已装配——见 coordinator.go:860 `SetCmdbarArrayHook` 处把同一 arrayHook 也写入
   `c.cmdbarValueExpander.ArrayHook`）。

- [ ] **Step 1: 写共享入口失败测试**

新建 `value_expand_array_test.go`，覆盖：纯文本→1 条；`$X`→1 条展开；`$CC`→1 条含 Actions；
`$AA("箭头","←↑→↓")`→4 条字符候选（GroupName=="箭头"）；`$SS(...)`→N 条元素候选；ArrayHook=nil 时
`$AA` 退化 1 条字面量。（用 dict 包内既有 marker 测试夹具风格，参考 `phrase_ss_test.go` 构造 hook。）

- [ ] **Step 2: 跑测试确认失败**

Run: `cd wind_input && go test ./internal/dict/ -run TestExpandToCandidates -v`
Expected: 编译失败 `ValueExpander has no method ExpandToCandidates`。

- [ ] **Step 3: 实现共享入口（提取 phrase 现有逻辑）**

把 phrase.go 数组展开逻辑迁入 `ExpandToCandidates`（保持纯函数化：输入 raw value + hooks，输出候选切片）。
`$CC`/`$X` 复用现有 `Expand` 单候选路径包一层成 1 元素切片。

- [ ] **Step 4: phrase.go 改调共享入口**

`SearchCommand` 数组分支替换为 `ExpandToCandidates`，删除迁走的重复实现。保留 cache（`cmdCache`）语义。

- [ ] **Step 5: 跑回归（关键）**

Run:
```bash
cd wind_input && go test ./internal/dict/ -run 'TestExpandToCandidates|SS|AA|Group|Phrase' -v
go test ./internal/dict/...
```
Expected: 新测 PASS；**所有现有 phrase / SS / AA / multigroup 测试不回归**。任何一条挂掉都说明等价
重构破坏了行为，必须修到全绿再继续。

- [ ] **Step 6: special-table 接数组**

`buildSpecialUICandidates` 改用 `ExpandToCandidates`；确保 `cmdbarValueExpander.ArrayHook` 已装配。
补一个 special 状态机测试：码表 `ar` → `$AA("箭头","←↑→↓")` → 进模式打 `ar` 显示 4 个箭头候选，
数字 `1` 选「←」上屏。

- [ ] **Step 7: 全量测试 + 编译 + 提交（建议节点）**

```bash
cd wind_input && go test ./internal/dict/... ./internal/coordinator/...
gofmt -w internal/dict/value_expand.go internal/dict/phrase.go internal/coordinator/handle_special_mode.go internal/dict/value_expand_array_test.go && go build ./...
git add internal/dict/value_expand.go internal/dict/phrase.go internal/coordinator/handle_special_mode.go internal/dict/value_expand_array_test.go
git commit -m "refactor(dict): $AA/$SS 数组展开抽共享入口，phrase 与特殊模式统一对接"
```

---

## 收尾：AGENTS.md 与全量验证

- [ ] 按 CLAUDE.md 约定更新受影响目录的 `AGENTS.md`：
  - `internal/coordinator/AGENTS.md`：新增 `handle_special_mode.go` / `special_mode_registry.go` /
    `special_mode_decide.go` 行；`triggerModes()` 描述补「special 实例动态插入」。
  - `internal/dict/AGENTS.md`：`value_expand.go` 描述补 `ExpandToCandidates` 多候选 + 数组统一。
  - `pkg/config`：若有 AGENTS.md，补 `SpecialModeConfig`。
  - 运行 `scripts/lint_agents_md.ps1` 检查引用不悬空。
- [ ] 全量：`cd wind_input && go test ./... && go build ./...`，`gofmt -l`（无输出）。
- [ ] 真机测试（用户执行）：手写一份 `schemas/special/symbols.dict.yaml`，配一个实例，验证
  prefix_free 自动上屏 / 定长 / 手动三档、命令候选、嵌入编码徽标提示、一次性退出。

## 不在本计划（后续 spec）

- 设置页 UI（实例增删改 + 码表编辑）。
- per-scheme 绑定与引擎门禁（`schemes` / `engines` 字段）。
- 符号入码字符集（`code_charset`）。
