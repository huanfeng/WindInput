<!-- Updated: 2026-06-08 -->

# 候选数量分档：基础档 + 扩展档（可统一对接的模式驱动）

## 背景与目标

当前"每页候选数"是**单一全局配置** `ui.candidates_per_page`（默认 7）。五笔用户普遍偏好较少候选以保持候选窗干净，但在以下场景又希望看到更多候选：

- 临时拼音（不确定编码时切拼音找字）
- 快捷输入（分号/z 等触发的快捷短语）
- `zzbd` 这类**码表内的快捷短语**（命中后是一批多字短语候选）
- 纯拼音方案（拼音引擎本身候选量大）

目标：让候选数量**按输入场景自动分档**，普通码表输入用"基础档"（少而干净），上述场景自动切到"扩展档"（多而全）。并且——**新增场景时有统一、低成本的对接方式**，不必每次改动核心判定逻辑。

## 现状分析：单一配置的贯穿链路

`candidatesPerPage` 是一个 `int`，从配置经 Coordinator 贯穿到分页、UI 渲染、提交记录三处：

| 位置 | 文件:行 | 作用 |
|------|---------|------|
| 配置字段 | `pkg/config/config.go:202` | `CandidatesPerPage int` |
| 默认值 | `pkg/config/config.go:455` | 默认 7 |
| 运行时字段 | `internal/coordinator/coordinator.go:227` | `candidatesPerPage int` |
| 初始化 | `coordinator.go:558-561` | 从 cfg 读，缺省 9 |
| 热更新 | `handle_config.go:22-23` | 配置变更时重算 |
| 分页总数 | `handle_candidates.go:511,770` | `totalPages` 计算 |
| 当前页切片 | `handle_candidates.go:546-547` | `startIdx/endIdx` |
| 发送 UI | `handle_candidates.go:615` | 传给 `sendCandidates` |
| 数字键选择 | `handle_key_event.go:720,747` | `pageStart = (page-1)*perPage` |
| 提交页内索引 | `coordinator.go:968,971` | `index % candidatesPerPage` |

**关键观察**：`candidatesPerPage` 与输入模式完全解耦，所有使用点都是"每次现取"。因此分档逻辑可**完全收敛在 Coordinator 层**，无需改动引擎层或 UI 层——只要把这些使用点统一改成调用一个"取有效值"的方法即可。

## 设计概览

```
                       ┌─────────────────────────────┐
  各输入模式/候选结果 ──▶│  extendedReasons (bitset)   │
                       └──────────────┬──────────────┘
                                      │ 只读
                       ┌──────────────▼──────────────┐
   分页/切片/选择/提交 ◀─│ effectiveCandidatesPerPage()│
                       └─────────────────────────────┘
```

- 保留 `candidates_per_page`（**基础档**）。
- 新增 `candidates_per_page_extended`（**扩展档**，`int`，`0` 表示禁用、始终用基础档）。
- 运行时维护一个位标志 `extendedReasons`，记录"当前有哪些原因要求扩展候选"。
- 所有使用点改调 `effectiveCandidatesPerPage()`：只要 `extendedReasons != 0` 且扩展档已配置，就返回扩展档，否则基础档。

## 配置层变更

### Go 端（`pkg/config/config.go`）

`UIConfig` 紧邻 `CandidatesPerPage` 新增：

```go
CandidatesPerPage         int `yaml:"candidates_per_page" json:"candidates_per_page"`
// CandidatesPerPageExtended 扩展档每页候选数。在临时拼音/快捷输入/短语/拼音引擎等
// 场景下生效。<=0 表示禁用扩展档（始终用基础档，向后兼容）；正值有效，上界
// clamp 到 15（与基础档上限一致）。判定"是否启用"统一由 >0 决定，故无需下界 clamp。
CandidatesPerPageExtended int `yaml:"candidates_per_page_extended,omitempty" json:"candidates_per_page_extended,omitempty"`
```

默认值（`config.go:455` 附近）：`CandidatesPerPageExtended: 0`（新装默认不启用，保持现有行为；用户主动配置后才分档）。

> **配置语义**：`0` = 关闭分档（向后兼容，老用户行为不变）。这避免了"新版本静默改变老用户候选数"的意外。

### 前端镜像（enum-constraint §前后端镜像要求）

- `wind_setting/frontend/src/api/settings.ts:115` 的 `UIConfig` 接口加 `candidates_per_page_extended: number;`
- 同文件 `:388` 附近 default 加 `candidates_per_page_extended: 0,`
- `wind_setting/frontend/src/schemas/appearance.schema.ts:44` 的"每页候选数"slider 下方新增一条 slider（详见下文 UI 段）。

## 运行时核心：extendedReasons 位标志

### 类型与字段（`coordinator.go`）

```go
// ExtendedCandidateReason 标记"当前为何需要扩展候选档"。
// 纯运行时内部状态，多个原因可同时成立，故用位标志。
// 注意：本类型**不序列化、不进 YAML、不跨进程**，因此不受 docs/design/enum-constraint.md
// 约束（该约束针对会进 YAML 的有限取值字符串配置）。1<<iota 是 Go bitset 标准惯用法。
type ExtendedCandidateReason uint32

const (
    ExtendedReasonTempPinyin  ExtendedCandidateReason = 1 << iota // 临时拼音模式
    ExtendedReasonQuickInput                                       // 快捷输入模式
    ExtendedReasonPinyinEngine                                     // 当前为拼音/混输引擎
    ExtendedReasonPhraseCands                                      // 当前候选含 PhraseLayer 短语
    // 未来新增场景：在此追加一行常量，effectiveCandidatesPerPage() 无需改动
)
```

Coordinator 新增字段（与 `candidatesPerPage` 相邻，`coordinator.go:227` 附近）：

```go
candidatesPerPage         int
candidatesPerPageExtended int
extendedReasons           ExtendedCandidateReason
```

### 单一裁判：effectiveCandidatesPerPage()

```go
// effectiveCandidatesPerPage 返回当前应使用的每页候选数。
// 这是分档的唯一裁判，新增扩展场景时**无需改动本函数**——
// 只需让对应场景在 extendedReasons 上置/清自己的位。
func (c *Coordinator) effectiveCandidatesPerPage() int {
    if c.candidatesPerPageExtended > 0 && c.extendedReasons != 0 {
        return c.candidatesPerPageExtended
    }
    return c.candidatesPerPage
}
```

辅助方法：

```go
func (c *Coordinator) setExtendedReason(r ExtendedCandidateReason)   { c.extendedReasons |= r }
func (c *Coordinator) clearExtendedReason(r ExtendedCandidateReason) { c.extendedReasons &^= r }
```

> **并发**：`set/clear/读取` 全部发生在 `HandleKeyEvent → ... → sendCandidates` 链路内，调用方已持 `c.mu`，无额外加锁需求。与现有 `candidatesPerPage` 的访问约束一致。

## 两类登记方式（统一对接契约）

审查中发现：四个 reason 的生命周期**不是同一种**，必须分两类登记，否则会出现"删回普通字但扩展档没收回"的 bug。

### A. 事件型（有明确激活/退出入口）

`set` 与 `clear` 严格配对，挂在模式的进入/退出处：

| Reason | 置位点 | 清位点 |
|--------|--------|--------|
| `ExtendedReasonTempPinyin` | `handle_temp_pinyin.go` 各 `tempPinyinMode = true` 处（:164, :332） | 各 `tempPinyinMode = false` 处（:218, :363）+ `clearState()` |
| `ExtendedReasonQuickInput` | 快捷输入激活入口（`quickInputMode = true`） | 退出入口 + `clearState()` |

> **兜底**：`clearState()`（`coordinator.go:976` 附近，重置全部输入态）统一 `c.extendedReasons = 0`，保证任何异常退出路径都不会残留事件型位。

### B. 派生型（每次候选刷新后重新评估）

无固定生命周期，是"当前查询状态"的派生属性。在 `updateCandidatesEx()` 末尾（`handle_candidates.go`，候选列表确定后）统一调用一次 `refreshDerivedExtendedReasons()` 重算：

```go
// refreshDerivedExtendedReasons 重算"可从当前查询状态派生"的扩展原因。
// 每次候选更新后调用，确保同一 composition 内增删字符时分档实时跟随。
func (c *Coordinator) refreshDerivedExtendedReasons() {
    // 引擎型：当前引擎是拼音/混输 → 置位，否则清位
    if c.engineMgr != nil {
        t := c.engineMgr.GetCurrentType()
        if t == engine.EngineTypePinyin || t == engine.EngineTypeMixed {
            c.setExtendedReason(ExtendedReasonPinyinEngine)
        } else {
            c.clearExtendedReason(ExtendedReasonPinyinEngine)
        }
    }
    // 内容型：候选列表含 PhraseLayer 短语（PhraseTemplate 非空）→ 置位，否则清位
    hasPhrase := false
    for i := range c.candidates {
        if c.candidates[i].PhraseTemplate != "" {
            hasPhrase = true
            break
        }
    }
    if hasPhrase {
        c.setExtendedReason(ExtendedReasonPhraseCands)
    } else {
        c.clearExtendedReason(ExtendedReasonPhraseCands)
    }
}
```

> 派生型必须 **set/clear 对称重算**（不能只 set），否则用户从 `zzbd`（短语）退格回普通码字时，扩展档不会收回。

### 新增场景的对接流程（这就是"统一对接方式"）

未来加新模式，只需判断它属于哪一类，二选一：

1. **事件型**（有明确进入/退出）：
   - 在 `ExtendedCandidateReason` 追加一个常量；
   - 在激活入口 `setExtendedReason(新常量)`，退出入口 `clearExtendedReason(新常量)`；
   - `clearState()` 已统一清零，无需额外处理。
2. **派生型**（从查询/候选状态可判断）：
   - 在 `ExtendedCandidateReason` 追加一个常量；
   - 在 `refreshDerivedExtendedReasons()` 加一段 set/clear 对称判断。

**两种情况都不需要改 `effectiveCandidatesPerPage()`**。这是本设计满足开闭原则的核心。

## 代码修改点清单

### 必改：使用点切换到 effectiveCandidatesPerPage()

将下列直接读 `c.candidatesPerPage` 的点替换为 `c.effectiveCandidatesPerPage()`：

- `handle_candidates.go:511` `totalPages` 计算
- `handle_candidates.go:546-547` `startIdx/endIdx` 切片
- `handle_candidates.go:615` 传 `sendCandidates` 的 perPage 参数
- `handle_candidates.go:770` 重算分页
- `handle_key_event.go:720,747`（及该文件其余数字键分支）`pageStart` 计算
- `coordinator.go:968,971` 提交时 `index % perPage`

> **一致性约束**：同一次按键处理内，分页切片、UI 发送、数字键选择、提交索引必须取**同一个** perPage 值，否则会出现"看到 9 个候选但按 7 翻页"的错位。由于 `effectiveCandidatesPerPage()` 是纯读且同一 composition 内 `extendedReasons` 稳定，同一次按键多次调用结果一致，天然满足。**例外**：派生型在 `updateCandidatesEx()` 末尾才刷新，需确认刷新发生在分页计算（:511）**之前**——见下"待确认"。

### 必改：配置与初始化

- `config.go` 加字段 + 默认值 + `Normalize`/兜底（参照 `MaxCandidateChars` 在 `config.go:625-627` 的越界回退写法）
- `coordinator.go:558-561` 初始化 `candidatesPerPageExtended`
- `handle_config.go:22-23` 热更新时同步 `candidatesPerPageExtended`

### 必改：前端三处（见配置层段）

### 文档同步（CLAUDE.md 要求）

- `internal/coordinator/AGENTS.md`：新增导出类型 `ExtendedCandidateReason` 与字段，需补一句说明。
- `pkg/config/AGENTS.md`（若存在枚举/字段清单）：新增 `CandidatesPerPageExtended`。
- 运行 `scripts/lint_agents_md.ps1` 校验引用不悬空。

## 设置页 UI

在 `appearance.schema.ts` 的"每页候选数"slider（`:44`）正下方新增：

```ts
{
  type: "slider",
  key: "ui.candidates_per_page_extended",
  label: "扩展候选数",
  hint: "临时拼音 / 快捷输入 / 短语等场景下的候选数；设为最小值表示与上面相同（关闭分档）",
  min: 0,            // 0 = 关闭分档
  max: 15,
  // 0 时 UI 展示 "跟随基础"，其余显示数字
}
```

> 文案与最小值含义需在 `general.search.ts`（搜索索引）同步登记，保持设置搜索可命中（参照现有 `candidates_per_page` 的登记）。

## 边界与测试

### 单元测试（`internal/coordinator`）

1. `extendedReasons == 0` 且扩展档已配 → 返回基础档。
2. 任一 reason 置位 + 扩展档已配 → 返回扩展档。
3. 扩展档 `= 0`（禁用）+ reason 置位 → 仍返回基础档。
4. 派生型对称性：模拟候选含/不含 `PhraseTemplate`，调用 `refreshDerivedExtendedReasons()` 后位正确置/清。
5. 事件型兜底：临时拼音激活置位 → `clearState()` 后 `extendedReasons == 0`。
6. 分页一致性：扩展档生效时 `totalPages`、切片、数字键 `pageStart` 用同一 perPage。

### 配置测试（`pkg/config/enums_test.go` 风格）

- YAML round-trip：`candidates_per_page_extended` 缺省（老配置）→ 读为 0，行为不变。
- 边界处理：`<=0` → 禁用（用基础档）；`>15` → clamp 到 15。

## 待实现阶段确认

1. **刷新时序**：确认 `refreshDerivedExtendedReasons()` 的调用点在 `updateCandidatesEx()` 内、且早于 `handle_candidates.go:511` 的 `totalPages` 计算（同一次按键内）。若分页计算在 `updateCandidatesEx()` 之外的更上层，需把刷新提到分页前。
2. **临时拼音融合入口**：临时拼音已"去模式化"为融合模型（拼音+码表候选融合）。`tempPinyinMode` 标志在 `handle_temp_pinyin.go` 仍真实 set/clear（z 触发路径已验证），但需排查是否存在**不置 `tempPinyinMode`** 的其它融合入口；若有，这类场景的扩展需求应改走派生型（按候选 `Source == 拼音` 判断）而非事件型。
3. **快捷输入子模式**：快捷输入内部可能再触发临时拼音子模式（`quickInputPinyinDictSwapped`）。两个事件型 reason 可同时置位，bitset 天然支持；确认退出子模式只 `clear` 自己的位，不误清外层 `ExtendedReasonQuickInput`。
4. **数字键边界**：`AllowSymbols` 开启时，数字键 1-9 "仅在索引超出当前页可见候选数时进 buffer"（`config.go:385`）。该判断也依赖 perPage，需一并改用 `effectiveCandidatesPerPage()`，避免扩展档下数字键行为错位。

## 影响面小结

- **新增**：1 个配置字段（Go + 前端镜像）、1 个运行时枚举类型、3 个 Coordinator 方法、1 条设置项。
- **修改**：约 8 处 `candidatesPerPage` 使用点改为方法调用。
- **不触碰**：引擎层、UI 渲染层、IPC 协议（`CandidatesPerPage` 仍按实际生效值透传，无需新增协议字段）。
- **向后兼容**：扩展档默认 0，老用户行为零变化。
