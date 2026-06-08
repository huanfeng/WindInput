<!-- Updated: 2026-06-08 -->

# 候选数量分档：基础档 + 扩展档（物化生效值 + 单一判定对接）

> **实现说明**：本文档为最终实现版。设计阶段曾考虑 `ExtendedCandidateReason` 位标志方案，
> 实现时发现一个更优解——**物化（materialize）字段**：复用现有 `candidatesPerPage` 字段
> 作为"当前生效值"，让几十个分页使用点零改动。本文已按物化方案重写。

## 背景与目标

当前"每页候选数"是**单一全局配置** `ui.candidates_per_page`（默认 7）。五笔用户普遍偏好较少候选以保持候选窗干净，但在以下场景又希望看到更多候选：

- 临时拼音（不确定编码时切拼音找字）
- 快捷输入（分号/z 等触发的快捷短语）
- `zzbd` 这类**码表内的快捷短语**（命中后是一批多字短语候选）
- 纯拼音方案（拼音引擎本身候选量大）

目标：让候选数量**按输入场景自动分档**，普通码表输入用"基础档"（少而干净），上述场景自动切到"扩展档"（多而全）。并且——**新增场景时有统一、低成本的对接方式**，不必每次改动核心判定逻辑。

> **分档原则**：只有「临时/特殊场景」才扩展（临时拼音、快捷输入、短语候选）。**引擎类型是常态属性，不参与分档**——混输/纯拼音是主力常态模式，若按引擎类型总是扩展，基础档将永远用不上，违背「保持干净」初衷。纯拼音用户想要更多候选可直接调大基础档。

## 现状分析：单一字段贯穿几十个分页点

`c.candidatesPerPage` 在 `internal/coordinator` 包里有**几十个使用点**，分散在 10+ 个文件（`handle_key_action.go`、`handle_clipboard.go`、`handle_candidate_action.go`、`handle_punctuation.go`、`handle_lifecycle.go`、`handle_quick_input.go`、`handle_candidates.go` 等），几乎全是 `(currentPage-1)*candidatesPerPage + ...` 的页偏移计算与页内切片。

**关键观察**：所有候选路径（主路径 `updateCandidatesEx`、临时拼音 `pinyin_mode_shared.go`、快捷输入 `handle_quick_input.go`、临时英文 `handle_temp_english.go`、二级展开）都**共用同一个 `c.candidatesPerPage` 字段**做分页。这个字段本质上已是"全局生效值"。

## 设计概览：物化方案

```
   各分页源头（candidates 确定后、totalPages 之前）
        │  调用 refreshEffectivePerPage()
        ▼
   ┌─────────────────────────────┐   读 base/extended 配置
   │ refreshEffectivePerPage()   │ + shouldUseExtendedCandidates()
   │  物化生效值 → candidatesPerPage│
   └──────────────┬──────────────┘
                  │ 写入
        ┌─────────▼─────────┐
        │ c.candidatesPerPage│ （生效值，整个 composition 期间稳定）
        └─────────┬─────────┘
                  │ 几十个分页/切片/选择/提交点直接读它（零改动）
                  ▼
        分页 / 切片 / 数字键选择 / 提交索引
```

- `candidatesPerPage` 字段语义从"配置值"升级为**"当前生效值"**（物化值）。
- 新增 `candidatesPerPageBase`（配置基础档）+ `candidatesPerPageExtended`（配置扩展档）。
- 在每条分页源头算 `totalPages` **之前**调一次 `refreshEffectivePerPage()`，把生效值物化到 `candidatesPerPage`。
- 几十个分页使用点**完全不动**——它们读 `candidatesPerPage` 读到的就是生效值。
- **一致性天然保证**：生效值在两次候选更新之间稳定，同一 composition 内分页/选择/提交读到的是同一个值，不会出现"显示 9 个却按 7 翻页"的错位。

## 配置层变更

### Go 端（`pkg/config/config.go`）

`UIConfig` 紧邻 `CandidatesPerPage` 新增：

```go
CandidatesPerPage int `yaml:"candidates_per_page" json:"candidates_per_page"`
// CandidatesPerPageExtended 扩展档每页候选数。在临时拼音/快捷输入/短语等场景下生效。
// <=0 表示禁用扩展档（始终用基础档，向后兼容）；正值有效，上界 clamp 到 10。
CandidatesPerPageExtended int `yaml:"candidates_per_page_extended,omitempty" json:"candidates_per_page_extended,omitempty"`
```

- 默认值：`CandidatesPerPageExtended: 0`（新装默认不启用，保持现有行为；用户主动配置后才分档）。
- 兜底（`ApplyConfigFallbacks`，参照 `MaxCandidateChars`）：`>10` 时 clamp 到 10；`<=0` 表示禁用，无需下界 clamp。

> **每页上限 10**：候选用数字键 1-9、0 选择，每页最多 10 个可选。超过 10 时第 11 个起既无对应数字键，`showUI` 的 `(i+1)%10` 标签还会与前面重复，故扩展档上限为 10。

> **配置语义**：`0` = 关闭分档（向后兼容，老用户行为不变），避免"新版本静默改变老用户候选数"。

### 前端镜像

- `api/settings.ts` 的 `UIConfig` 接口加 `candidates_per_page_extended: number;`，default 加 `candidates_per_page_extended: 0`。
- `schemas/appearance.schema.ts` 的"每页候选数"slider 下方新增一条 slider（详见 UI 段）。
- 设置搜索索引 `general.search.ts` 从各 PageSchema 的 label/hint **自动派生**，新 slider 自动收录，无需手动登记。

## 运行时核心

### 字段（`coordinator.go`）

```go
// candidatesPerPage 是「当前生效」的每页候选数（物化值），所有分页/选择/切片逻辑读它。
candidatesPerPage int
// 用户配置的基础档 / 扩展档（只读，生效值物化到 candidatesPerPage）。
candidatesPerPageBase     int
candidatesPerPageExtended int
```

初始化（`NewCoordinator`）读 `cfg.UI.CandidatesPerPage`/`CandidatesPerPageExtended` 填 base/extended，`candidatesPerPage` 初始物化为 base。

### 物化函数（`handle_candidates.go`）

```go
// refreshEffectivePerPage 把「当前生效」的每页候选数物化到 c.candidatesPerPage。
// 必须在每条分页源头计算 totalPages 之前调用。
func (c *Coordinator) refreshEffectivePerPage() {
    base := c.candidatesPerPageBase
    if base <= 0 {
        base = 7 // 兜底，与历史默认一致
    }
    if c.candidatesPerPageExtended > 0 && c.shouldUseExtendedCandidates() {
        c.candidatesPerPage = c.candidatesPerPageExtended
    } else {
        c.candidatesPerPage = base
    }
}
```

### 唯一对接点：shouldUseExtendedCandidates()

```go
// shouldUseExtendedCandidates 判定当前输入场景是否需要「扩展档」候选数。
// 这是候选数分档的唯一对接点：未来新增需要更多候选的模式，只需在此追加一个判断分支，
// 无需改动 refreshEffectivePerPage 或任何分页使用点。
//
// 仅覆盖「临时/特殊场景」；常态打字（含混输/纯拼音引擎）一律用基础档——
// 引擎类型是常态属性，不参与分档。
func (c *Coordinator) shouldUseExtendedCandidates() bool {
    if c.tempPinyinMode || c.quickInputMode { // 事件型：直接读已有模式标志
        return true
    }
    for i := range c.candidates { // 内容型：候选含 PhraseLayer 短语（如 zzbd）
        if c.candidates[i].PhraseTemplate != "" {
            return true
        }
    }
    return false
}
```

> **为何不用位标志**：设计阶段曾设想 `ExtendedCandidateReason` bitset + set/clear 配对。实现时发现
> `tempPinyinMode`/`quickInputMode` 本就是可随时读取的持久字段，短语候选也可从当前候选列表
> 直接派生——所有"原因"都是**可派生的**，于是 bitset 退化成一个布尔或。`shouldUseExtendedCandidates()`
> 每次物化时全量重算，比维护 bitset 生命周期更简单，且天然修复"删回普通字扩展档不收回"的隐患
> （每次候选刷新都重算，派生原因消失即收回）。

> **并发**：物化与读取全部发生在 `HandleKeyEvent` 持 `c.mu` 的链路内，无额外加锁需求。

### 新增场景的对接流程（"统一对接方式"）

未来加新模式，只需在 `shouldUseExtendedCandidates()` 里加一行判断：

```go
if c.someNewMode { return true }   // 事件型：读新模式标志
// 或内容型：在候选遍历里加一个属性判断
```

**不需要改 `refreshEffectivePerPage()`，不需要改任何分页使用点。** 这是本设计满足开闭原则的核心。

## 代码修改点清单（实际）

### 配置与初始化

- `config.go`：新增 `CandidatesPerPageExtended` 字段 + 默认值 0 + `>10` clamp 兜底。
- `coordinator.go`：字段定义新增 base/extended；`NewCoordinator` 读配置初始化。
- `handle_config.go`：热更新写 `candidatesPerPageBase`/`candidatesPerPageExtended` 后调 `refreshEffectivePerPage()` 再重算分页。

### 物化函数 + 判定函数

- `handle_candidates.go`：新增 `refreshEffectivePerPage()` + `shouldUseExtendedCandidates()`。

### 各分页源头插入物化调用（在 `totalPages` 计算之前）

| 路径 | 文件 | 场景 |
|------|------|------|
| 主路径 | `handle_candidates.go`（updateCandidatesEx 末尾） | 码表/混输/拼音引擎 + 短语 |
| 二级展开 | `handle_candidates.go`（展开候选重算分页） | 字符组展开含短语 |
| 临时拼音 | `pinyin_mode_shared.go`（替换原 `if<=0{=7}` 兜底） | tempPinyinMode |
| 快捷输入 | `handle_quick_input.go`（替换原 `if<=0{=7}` 兜底） | quickInputMode |
| 临时英文 | `handle_temp_english.go` | 不触发扩展，但需收回上一模式残留值 |
| 热更新 | `handle_config.go` | 配置变更实时重算 |

> **几十个页偏移使用点零改动**——这是物化方案相对"每点改调 effectiveCandidatesPerPage()"的核心优势：改动面小、一致性天然。

### 文档同步

- `internal/coordinator/AGENTS.md`：`handle_candidates.go` 条目补充分档机制说明（物化 + 唯一对接点）。
- `pkg/config/AGENTS.md` 不存在；`CandidatesPerPageExtended` 是普通 int 字段，非枚举，不受 `enum-constraint` 约束。

## 设置页 UI

`appearance.schema.ts` 的"每页候选数"slider 正下方新增：

```ts
{
  type: "slider",
  key: "ui.candidates_per_page_extended",
  label: "扩展候选数",
  hint: "临时拼音 / 快捷输入 / 短语等场景下的每页候选数；设为 0 表示与上面相同（关闭分档）",
  min: 0,
  max: 10,
  step: 1,
  displayValue: (v) => (v <= 0 ? "跟随基础" : `${v} 个`),
}
```

## 边界与测试

### 单元测试（`internal/coordinator/candidate_perpage_test.go`，已实现）

`TestRefreshEffectivePerPage` 表驱动覆盖：
1. 扩展档禁用（=0）+ 临时拼音 → 仍用基础档。
2. 临时拼音 / 快捷输入 / 短语候选 → 切扩展档。
3. 普通候选无原因 → 基础档。
4. base 配置 0 → 兜底回退 7；base=0 但有原因 → 仍用扩展档。

`TestRefreshEffectivePerPage_RecoversExtendedValue`：含短语候选物化为扩展档后，退回普通候选重算应**收回基础档**（验证派生原因消失即收回，修复隐患）。

### 配置测试

`candidates_per_page_extended` 缺省（老配置）→ 读为 0，行为不变（由 config 包结构体序列化测试覆盖）。

## 实现阶段已确认结论

1. **刷新时序** ✅：真正的分页发送点是 `showUI()`，`updateCandidatesEx()` 在候选确定后、`totalPages` 计算前调 `refreshEffectivePerPage()`，时序正确。
2. **临时拼音融合入口** ✅：`tempPinyinMode` 在 `handle_temp_pinyin.go` 真实 set/clear；临时拼音候选走 `pinyin_mode_shared.go` 分页路径，已接入物化调用。
3. **快捷输入子模式** ✅：物化方案不用 bitset，`shouldUseExtendedCandidates()` 每次全量重算，子模式嵌套无"误清外层位"问题。
4. **数字键边界**：`AllowSymbols` 数字键判断读 `c.candidatesPerPage`（生效值），随物化自动一致，无需单独改动。
5. **临时英文一致性** ✅：临时英文不触发扩展，但仍调 `refreshEffectivePerPage()` 以收回上一模式可能残留的扩展值。

## 影响面小结

- **新增**：1 个配置字段（Go + 前端镜像）、2 个 Coordinator 字段、2 个 Coordinator 方法、1 条设置项。
- **修改**：6 处分页源头插入物化调用；初始化/热更新写 base/extended。
- **零改动**：几十个页偏移/切片/选择使用点（复用 `candidatesPerPage` 生效值）。
- **不触碰**：引擎层、UI 渲染层、IPC 协议（`CandidatesPerPage` 按生效值透传，无新协议字段）。
- **向后兼容**：扩展档默认 0，老用户行为零变化。
