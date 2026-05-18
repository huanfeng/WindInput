# 候选调整目标 (Candidate Actions)

> 候选词右键菜单 / 数字键热键的"前移 / 后移 / 置顶 / 删除 / 恢复默认"操作的完整设计目标。
> 修改候选调整逻辑前**必须**先回查本文档矩阵。

## 1. 候选类型矩阵

候选 (`candidate.Candidate`) 来自不同源, 字段组合区分 6 类:

| 类型 | IsGroup | IsGroupMember | IsPhrase | IsCommand | ID 形态 | PhraseTemplate | GroupTemplate | 例子 |
|---|---|---|---|---|---|---|---|---|
| **A 码表/用户词** | - | - | - | - | 空 | 空 | 空 | `nihao`→你好 (五笔/拼音/用户词) |
| **B 拼音候选** | - | - | - | - | 空 | 空 | 空 | `nh`→你好 (拼音引擎产出) |
| **C 普通短语** | - | - | ✓ | ✓ | `phrase:<code>:<template>` | `<template>` | 空 | `date`→2026-05-18 (静态/动态非组短语) |
| **D 组成员**<br>(展开后) | - | ✓ | ✓ | ✓ | `phrase:<code>:<elem>` | `<elem>` | `<groupRecordText>` | `zzsz`→①、`zzbd`→，(展开后单字符/单元素) |
| **E Group nav**<br>(未展开 / collapse) | ✓ | - | ✓ | - | `phrase:<code>:<groupTemplate>` | `<groupTemplate>` | `<groupTemplate>` | `zz` 前缀下"圆数字"、`zzbd` mixed collapse 出的"标点符号" |
| **F cmdbar 命令** | - | - | ✓ | ✓ | `phrase:<code>:<template>` | `<template>` | 空 | `coen`→切中英、`cobd`→打开百度 (Actions 非空) |

**关键标记说明:**

- `ID` 形态遵循 `phrase:<code>:<template>` 命名空间。E 类型的 `template` 是 group 原始 PhraseRecord.Text (含 `$AA(...)` / `$SS(...)` marker), 不是显示名 (避免跨语言显示名变更破坏 Shadow 匹配)。
- `GroupTemplate` 字段在 D / E 类型才有值: D 留给 E 在 collapse 时反推 (`collapseGroupMembersIfMixed` 用 first member 的 `GroupTemplate` 生成 nav 的 `ID` / `PhraseTemplate`)。
- F (cmdbar) 与 C (普通短语) 在 ID 形态和 PhraseLayer 路径上完全等价, 唯一差异是 `Actions` 字段非空 (PhraseLayer.expandDynamicEntry 把 cmdbar marker 解析为 ResolvedAction)。所以 F 的所有候选调整操作**等同于 C** — 不再有"cmdbar 不能 pin"的限制。

## 2. 操作权能矩阵

四个候选调整操作 × 六类候选:

| 操作 | A 码表/用户词 | B 拼音 | C 普通短语 | D 组成员 | E Group nav | F cmdbar |
|---|---|---|---|---|---|---|
| **前移/后移** | ✓ Shadow pin (word) | ✗ (拼音 weight 不稳定, 无法 pin 到稳定位置) | ✓ Shadow pin (id) | **✗ 禁用** | ✓ Shadow pin (id) | ✓ Shadow pin (id) |
| **置顶** | ✓ | ✗ | ✓ | **✗ 禁用** | ✓ | ✓ |
| **删除** | ✓ Shadow.delete (或源词库 Remove) | ✓ Shadow.delete | ✓ `DisablePhrase` (软删 `PhraseRecord.Enabled=false`) | **✗ 禁用** | ✓ `DisablePhrase` (同 C) | ✓ `DisablePhrase` (同 C) |
| **恢复默认** | shadow **pin** 存在时启用 | 同左 | 同左 | **永远 disabled** | 同左 | 同左 |

**横向规则:**

- **"恢复默认"语义仅针对位置调整 (pin)**: 即只回滚 `Shadow.Pinned`。被 delete/disable 的候选用户在 IME 里看不到, 也触达不到右键菜单, 所以"恢复删除"只能通过设置 UI ("Switch Enabled" 或"Shadow 规则列表"页面) 操作。
- **首位/末位的常规禁用**: "前移在第 1 位 disable", "后移在末位 disable", "置顶在第 1 位 disable" — 跨所有类型生效, 不变。
- **拼音模式下码表候选**: 拼音引擎下的 A 类型候选 (B) 不允许位置调整, 因为拼音的 weight 计算是 rimeScore × 1M, 跟 Shadow pin Position 不兼容; 但拼音模式下若有 IsCommand 候选 (C/F), 仍允许调位置 (走 phrase id 路径)。

### 2.1 右键"删除"菜单文案 (动态化)

由于"删除"对不同候选实际是三种不同操作 (真删 / Shadow 隐藏 / PhraseRecord 禁用), 菜单文案按候选类型动态显示, 让用户明确即将发生的事:

| 候选类型 | 菜单文案 | 实际操作 | 可恢复路径 |
|---|---|---|---|
| C 普通短语 / E nav / F cmdbar | **禁用短语(X)** | `DisablePhrase` (`PhraseRecord.Enabled=false`) | 设置 UI → 短语列表 → 启用 Switch |
| A 用户词 (Meta.IsUserDict) | **删除用户词(X)** | 用户词库 `Remove` (真删, 不可恢复) | 重新输入加词流程 |
| A 临时词 (Meta.IsTempDict) | **删除临时词(X)** | 临时词库 `Remove` (真删) | 临时词本身设计就是短生命周期, 不需恢复 |
| A 系统码表 / B 拼音 | **隐藏候选(X)** | `Shadow.delete` | 设置 UI → Shadow 规则列表 → 移除规则 |
| D 字符组成员 | (disabled, 文案兜底 "删除词条") | (操作不可用, 改 yaml) | N/A |

UI 层 `computeDeleteMenuLabel(isPhrase, isUserDict, isTempDict, isGroupMember)` 按优先级返回标签 (短语 > 用户词 > 临时词 > 默认隐藏 > 组成员兜底), 实现单测见 `internal/ui/menu_disable_test.go::TestComputeDeleteMenuLabel`。

## 3. Shadow 应用时序

候选调整通过 Shadow 层 (`internal/dict/store_layer.go` + `internal/store/shadow.go`) 实现, 应用时机有两个阶段:

### 3.1 引擎 Phase 6 (`ApplyShadowPins`)

在 mixed/codetable/pinyin engine 内, `ConvertEx` 排序完成后立即应用:

```
ConvertEx
 ├─ 各 layer 出候选
 ├─ 合并 + sort by weight
 ├─ filter / dedupe
 └─ ApplyShadowPins(input, GetShadowRules(input))  ← Phase 6
```

`ApplyShadowPins` 是幂等的:
1. 先按 `Deleted` 规则过滤 (单字保护仍在内部生效)
2. 再按 `Pinned` 规则把命中候选放到 `Position` 槽位 (碰撞顺延)
3. 剩余 unpinned 填空隙

匹配优先级 (R2 后):
- `rule.CandID` 非空 → 按 `cand.ID == p.CandID` (动态短语跨日子稳定)
- `rule.CandID` 空 → 按 `cand.Text == p.Word` (兼容手输文本规则)

### 3.2 Coordinator collapse 后二次应用 (bug 2 修复)

`coordinator/handle_candidates.go::updateCandidatesEx` 在引擎结果之后做了两步后处理:

```
result.Candidates (引擎已应用 Phase 6 Shadow)
 ├─ expandAACandidates  (用户词库 $AA 字面 marker 展开为 N 字符 / nav)
 ├─ collapseGroupMembersIfMixed (多 group/混合时收成 nav)
 └─ ApplyShadowPins(c.inputBuffer, ...)  ← coordinator 二次应用
```

**为什么需要二次应用**: nav (E 类型) 是 coordinator 在引擎之后 collapse 出来的, 引擎 Phase 6 看不到 nav 候选, 所以用户对 nav 的 pin 在 Phase 6 阶段无候选可匹配, 失效。在 collapse 之后再调一次 `ApplyShadowPins`, 此时 nav 已存在 (带稳定 ID), 能正确命中并放置到 pin Position。

**为什么是幂等的**: `ApplyShadowPins` 实现里每次都是"从规则重新计算槽位", 不依赖输入候选的当前位置。第二次调用对已经按 Phase 6 pin 过的候选 (A/C/F) 重新走一次匹配, 结果完全相同 (规则相同 → 位置相同)。引擎层的 pin 不会被破坏。

详见 `mixed.go::convertMixed` 中"ApplyShadowPins 是幂等的"注释。

## 4. HasShadow 查询: `HasShadowPin` vs `HasShadowRule`

右键菜单"恢复默认"启用条件取决于候选是否有可恢复的 Shadow 覆盖。按 §2 的语义"恢复默认仅针对位置调整", 查询需要拆分:

| 函数 | 查询内容 | 用途 |
|---|---|---|
| `HasShadowPin(code, word, candID)` | 仅 `rules.Pinned` | 右键菜单"恢复默认"启用条件, `cand.HasShadow` 字段填充 |
| `HasShadowRule(code, word, candID)` | `rules.Pinned` + `rules.Deleted` | 设置 UI 通用判断、debug 工具 |

**handle_candidates.go 跳过条件**:

```go
// 旧 (bug 1 根因): nav 被跳过 → cand.HasShadow 永远 false → "恢复默认" 永远 disabled
if dictMgr != nil && !cand.IsGroup {
    cand.HasShadow = dictMgr.HasShadowRule(...)
}

// 新: D (组成员) 跳过, nav (E) 参与查询, 与 §2 矩阵一致
if dictMgr != nil && !cand.IsGroupMember {
    cand.HasShadow = dictMgr.HasShadowPin(c.inputBuffer, cand.Text, cand.ID)
}
```

D 类型菜单全 disable, HasShadow 查询无需触发 (跳过节省调用)。

## 5. Nav 候选的 ID / PhraseTemplate / GroupTemplate

Nav 候选必须有稳定 ID 以承担 pin/delete 的跨 collapse 状态匹配。命名规约:

```
nav.ID             = "phrase:" + groupCode + ":" + groupTemplate
nav.PhraseTemplate = groupTemplate
nav.GroupTemplate  = groupTemplate
```

其中 `groupTemplate` 是 group 原始 `PhraseRecord.Text` 字符串 (含 `$AA("name", "chars")` 或 `$SS(...)` marker)。**不**用 `group.Name` (显示名), 因为显示名跨语言/用户配置可能变化, 而 marker 文本本身就是 group 的 stable 主键 (PhraseRecord 按 `(code, text)` 唯一)。

**4 处 nav 生成点**:

| 文件:函数 | 路径 | groupTemplate 来源 |
|---|---|---|
| `phrase.go::SearchPrefix` (phraseGroups 路径) | 用户输入是 group code 严格前缀 | 反查 `staticPhrases[code]` 或 `dynamicPhrases[code]` 的对应 PhraseEntry.Text |
| `phrase.go::SearchCommand` (navResults 路径) | 用户输入是某 group code 严格前缀 (精确命中后回退) | 同上 |
| `coordinator/handle_candidates.go::expandAACandidates` (prefix 分支) | 用户词库存 `$AA(...)` 字面 text | `cand.Text` 本身就是 marker |
| `coordinator/handle_candidates.go::collapseGroupMembersIfMixed` | 引擎已展开为 group member, 后处理 collapse | 从 first member 的 `cand.GroupTemplate` 继承 |

**D 类型 (group member) 必须填 GroupTemplate**: 让 collapse 时能反推 nav 的 ID/PhraseTemplate。这是引入 `candidate.GroupTemplate` 字段的根本原因。

## 6. 实现取舍

### 6.1 为什么 D 类型 (组成员) 全 disable

- 字符组 `$AA(name, chars)` 中字符顺序由 yaml 数组定义, **结构上**已经是稳定顺序
- 允许 pin/delete 个别字符等于在 Shadow 层维护跟 yaml 平行的字符顺序映射, 双轨漂移
- 用户改字符组应该改源 yaml (设置 UI 短语编辑器), 而不是在 IME 里点点点
- 未来 TODO 见 §7

### 6.2 为什么二次 `ApplyShadowPins` 而不用 first member pin 注入

替代方案 B: pin nav → 隐式 pin 该 group 任一 member 到同位置 → collapse 时 nav 继承位置

**否决理由**:
- 跨 schema 同步时, "代表 member" 选哪个? 显示名/yaml 顺序变化时映射不稳定
- ApplyShadowPins 已经是幂等的, 二次调用零额外状态 (规则一致 → 输出一致)
- 让 nav 用自己的稳定 ID 直接被 pin, 语义最干净

二次应用的代价: O(N×M) 一次额外扫描 (N=候选数, M=pin 规则数), 候选量 < 5000、pin 规则量 < 100 时实测延迟 < 1ms。可接受。

### 6.3 为什么 HasShadowRule 不直接改, 而是新增 HasShadowPin

- HasShadowRule 还有其它调用点 (设置 UI 的"是否有 shadow"统计、debug 命令), 它们需要包含 delete 的语义
- 把语义分裂成两个明确函数, 调用方按需选择, 避免"同一函数在不同上下文行为不同"

### 6.4 为什么 F (cmdbar) 跟 C 一致

- F 和 C 在 PhraseLayer 里**走同一路径** (`expandDynamicEntry` 区分 `$CC` marker 与否)
- F 的 ID 命名空间和 C 完全一致 (`phrase:<code>:<template>`)
- 早期热键 `len(cand.Actions) > 0` disable 是历史残留 (担心 Actions 改变时 pin 不稳定), R2 后已用 ID 匹配, 不再是问题
- 用户对 cmdbar 命令 (如 `cobd` 打开百度) 调位置是合理需求

## 7. 后续修改注意点 + 未来 TODO

### 7.1 修改候选调整逻辑时必读

1. **新增候选类型** → 必须同步本文档 §1/§2 矩阵, 并在 `menu_disable_test.go` 加测试覆盖
2. **新增 Shadow 规则字段** → 必须同步 `ApplyShadowPins` 匹配逻辑 + `HasShadowPin` / `HasShadowRule` 两处查询
3. **改 nav 生成路径** → 必须保证 ID/PhraseTemplate/GroupTemplate 三个字段一致, 否则 collapse 后 pin 失效
4. **改 collapse 逻辑** (`collapseGroupMembersIfMixed`) → 必须保留二次 `ApplyShadowPins` 在 collapse 之后调用, 否则 bug 2 复现

### 7.1.1 多 group 同 code 数据模型 (2026-05-18 升级)

`PhraseLayer.phraseGroups` 类型升级为 `map[string][]PhraseGroup`, 允许同一 `code` 注册多个 `$AA` / `$SS` group。配套:

- `PhraseEntry.GroupRawText` 字段反查归属 group (LoadFromStore 时填), 让 `staticPhrases` / `dynamicPhrases` 中混杂的多 group 成员能按 group 分组
- `SearchCommand` 精确码命中: 遍历所有 group, append 成员候选 (每条带各自 `GroupTemplate`), 单 group 时直接展示, 多 group 时由 coordinator collapse 出多 nav
- `SearchPrefix` / `SearchCommand` 前缀路径: 每 group 各出 1 个 nav 候选
- `collapseGroupMembersIfMixed` 分组 key 用 `GroupTemplate` (而不是 `GroupCode`), 让同 code 多 group 各自独立 collapse
- `Coordinator.expandedGroupTemplate` 状态机字段 (旧 `expandedGroupCode` 重命名): 用户主动选中某 nav 后, 二级展开模式按 `GroupTemplate` 跟踪, 同 code 多 group 之间能独立切换
- nav `Comment` 统一显示 `(N 项)` (旧 `N 字`), 字符组 / 字符串组用同一表达
- 测试覆盖: `internal/dict/phrase_multigroup_test.go`

### 7.2 未来 TODO

- **D 类型原地编辑**: 字符组成员在 IME 内右键 → 弹出"编辑该字符组"对话框 → 允许用户改 chars 数组顺序 (不走 Shadow), 提交后写回源 PhraseRecord.Text。预计需要 RPC 新增 `Phrase.EditGroupMarker` 接口 + 设置 UI 借用既有短语编辑器。注释标记: 搜索 `TODO: 未来支持组内成员原地编辑`
- **拼音 B 类型位置调整**: 当前 disable, 因为拼音 weight ≠ pin position。可考虑给拼音候选也用 phrase 风格 ID (按 `pinyin:<code>:<text>`), 走 Shadow pin 路径, 但需要确认引擎排序时的 weight 兼容性。
- **跨方案 pin**: 当前 Shadow pin 在方案桶, 切方案后不可见。如有用户反馈, 考虑在 nav/普通短语候选上做"全局 pin"额外维度 (跟 DisablePhrase 的全局语义一致)。

## 8. 测试矩阵

| 文件 | 覆盖 |
|---|---|
| `internal/ui/menu_disable_test.go` | §2 操作权能矩阵 × 类型 (尤其 D 全 disable / E 全允许 / F 跟 C 一致) |
| `internal/coordinator/collapse_groups_test.go` | collapse 后二次 ApplyShadowPins 让 nav pin 生效 (bug 2 回归) |
| `internal/dict/manager_test.go` | HasShadowPin 仅查 Pinned, HasShadowRule 同时查 Pinned + Deleted |
| `internal/dict/composite_shadow_test.go` | ApplyShadowPins 幂等性 (调两次结果相同) |

## 相关文档

- `docs/design/2026-05-12-command-bar-design.md` — cmdbar 整体架构 ($CC/$AA/$SS marker)
- `docs/design/2026-05-16-cmdbar-followup.md` — R2 Candidate.ID + Shadow CandID, weight tier 设计
- `internal/candidate/AGENTS.md` — Candidate 字段速查
- `internal/dict/AGENTS.md` — Shadow / PhraseLayer 接口
- `internal/coordinator/AGENTS.md` — collapse 状态机 + expandedGroupCode
