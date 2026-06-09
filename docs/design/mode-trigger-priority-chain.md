# 模式激活键：顶码上屏与优先级回落链

## 背景与需求

输入法有多个「由触发键激活的模式」：**快捷输入**、**临时拼音**、**临时英文**（未来还会有**生僻字模式**、**符号码表模式**）。当前这些模式的触发判定（`getQuickInputTriggerKey` / `getTempPinyinTriggerKey` / `getTempEnglishTriggerKey`）结构同构，都靠一道**互斥门禁**避让候选选择：

```go
if len(c.inputBuffer) > 0 || len(c.candidates) > 0 { return "" }
```

即「正在输入（有 buffer / 有候选）时，模式键一律不触发」。结果是：正在输入中文时按引导符（如 `` ` ``），中文会被当标点顶字上屏、引导符字符也一并上屏，**且不进入对应模式**。

**期望**：正在输入中文时按模式激活键 → 当前中文（顶码 / 高亮候选）上屏 → 直接进入该模式，触发键字符本身不上屏。

放开「有候选时也激活模式」后，模式激活键会与**二/三候选选择键**（如 `;` `'`）冲突——它们原本靠 `candidates==0` 门禁互斥。因此需要把「互斥避让」改造成一条**优先级清晰的回落链**。

## 已确认的需求边界

| 决策点 | 结论 |
| --- | --- |
| 适用模式 | **全部触发键激活的模式**统一：快捷输入 / 临时拼音 / 临时英文 + 未来模式 |
| 二/三候选键 vs 模式激活键 | **二/三候选键优先级更高**（仅在候选数量足够时生效） |
| 候选不足时 | 二/三候选键无效 → 回落到**模式激活** → 仍不接则回落**普通符号** |
| 空码场景（buffer 非空但无有效候选） | 丢弃无效编码，直接进模式 |
| 是否依赖 `punct_commit` 开关 | **独立**——模式激活总是先顶码再进 |
| 顶码上屏提交哪个候选 | **当前高亮候选** |
| 架构形态 | **集中回落链 + 轻量模式表** |
| 模式间优先级 | **快捷输入 > 临时拼音 > 临时英文**；未来模式插在「临时拼音之后、临时英文之前」 |
| 适用引擎 | 各模式沿用既有引擎门禁（如临时拼音仅码表引擎） |

## 统一优先级回落链

正在输入（`inputBuffer` 非空 / 有候选）时，一个 `!hasShift` 的键按下，依次尝试：

| 优先级 | 角色 | 生效条件 | 动作 |
| --- | --- | --- | --- |
| A | 双拼韵母键 | 双拼模式 + 有 buffer + 该键是当前方案韵母键 | `handleAlphaKey`（送引擎，沿用现状） |
| B | 二候选键 | 候选数 ≥ 2 | 选第 2 候选 |
| C | 三候选键 | 候选数 ≥ 3 | 选第 3 候选 |
| D | **模式激活键** | 该键绑定某启用的模式（按模式表顺序） | **顶码上屏当前高亮候选 + 进入该模式** |
| E | 二/三候选键回落 | 该键是二/三候选键但候选不足且**非**模式键 | `handleOverflowSelectKey`（沿用现状） |
| F | 普通标点/符号 | — | `handlePunctuation` |

关键点：
- B/C 在 D 之前 → **二三候选键优先于模式激活键**。同一键（如 `;` 既是二候选键又是模式键）：候选 ≥ 2 时选候选（B），候选不足时回落到模式激活（D）。
- D 在 E 之前 → 二三候选键候选不足时，若**也**绑定了模式 → 走模式激活（D）；否则才走原 overflow 策略（E）。
- 高亮候选是 **cmdbar 命令候选（`Actions` 非空）或组候选（`IsGroup`）** 时，**不走 D**，回落到 F（标点管线，与改动前一致），避免触发命令副作用 / 导航语义复杂化。

### 模式间优先级（写入文档，便于未来插入）

模式表为**有序列表**，匹配时按序命中第一个启用且匹配的模式：

```
1. 快捷输入 (quick_input)
2. 临时拼音 (temp_pinyin)
   └─ ★ 未来模式（生僻字 rare_char、符号码表 symbol_table…）插入此处
3. 临时英文 (temp_english)
```

## 方案选型

### 采用：集中回落链 + 轻量模式表

- **轻量模式表**：用一个有序切片描述所有触发键激活的模式，每项提供「纯触发键匹配」「是否启用」「进入模式的状态设置 + preedit 前缀」三个函数字段。不引入完整 interface 抽象。
- **集中回落链**：新建单一路由函数，把 A~F 优先级写在一处，可读、可测、易扩展。
- 三段重复的「键→VK」`switch` 收敛为一个公共 helper。

### 否决
- **完整模式注册表 interface**：改动面与回归风险过大，超出当前需求。
- **渐进补丁（各 `getXxxTriggerKey` 内加让位门禁）**：优先级逻辑分散，未来每加一个模式都要重复，违背「优先级标记清楚」诉求。

## 详细设计

### 1. 公共触发键匹配 helper
（`internal/coordinator/`，新增）

抽出三个模式重复的 `switch tkKey { case KeyGrave/KeySemicolon/... }`：

```go
// matchTriggerKeyInList 判断 (key,keyCode) 是否匹配 triggerKeys 列表中的某个键，
// 返回匹配到的配置项字符串（空串=未匹配）。纯键匹配，不含任何状态门禁。
func matchTriggerKeyInList(triggerKeys []string, key string, keyCode int) string
```

各模式的 `matchXxxTrigger` 改为：判 `enabled`（配置/引擎门禁）+ 调 `matchTriggerKeyInList`。去掉内部的 `len(inputBuffer)/len(candidates)` 状态门禁——状态优先级交由回落链统一裁决。

> 临时拼音的 `;`/`'` 旧有 `candidates==0` 内联门禁随之移除（回落链的 B/C 优先级已保证候选足够时不会进临时拼音）。
> `z` 键触发**不纳入**本回落链：`z` 是字母键，buffer 非空时走 `handleAlphaKey` / `zHybridFallback` 的既有独立路径，保持现状。

### 2. 轻量模式表
（`internal/coordinator/`，新增）

```go
type triggerModeEntry struct {
    name    string                                   // "quick_input" / "temp_pinyin" / "temp_english"
    match   func(key string, keyCode int) string     // 含 enabled 判定；返回 triggerKey（空=不匹配）
    setup   func(triggerKey string) string           // 设置模式状态(+armPendingFirstShow)，返回 preedit 前缀
}

// triggerModes 按优先级返回模式表。未来模式插在 temp_pinyin 之后、temp_english 之前。
func (c *Coordinator) triggerModes() []triggerModeEntry
```

`setup` 复用各模式现有进入逻辑中「状态设置」部分（从 `enterXxxMode` 抽出，不含 result 构造）：
- 快捷输入：`quickInputMode=true` / ForceVertical 布局 / `updateQuickInputCandidates` / `armPendingFirstShow`，返回 `quickInputPrefix()`。
- 临时拼音：`EnsurePinyinLoaded` / `ActivateTempPinyin` / `tempPinyinMode=true` / reset buffer / `armPendingFirstShow`，返回 `tempPinyinPrefix()`。
- 临时英文：对应状态设置，返回前缀。

现有 `enterXxxMode`（空 buffer 进入）保留为薄封装：调 `setup` 后 `return modeCompositionResult(prefix, len(prefix))`。

### 3. 集中回落链路由
（`internal/coordinator/handle_key_event.go`）

新增 `routeBufferedTriggerKey(key string, data *bridge.KeyEventData) *bridge.KeyEventResult`，返回 `nil` 表示「本链未处理，调用方继续后续逻辑」。

调用点：替换现有 `:498-510` 的三段模式激活 `if`，并前移到二三候选 `switch` 之前的统一入口；当 `!hasShift && c.chineseMode && (len(inputBuffer)>0 || len(candidates)>0)` 时优先调用：

```go
if !hasShift && c.chineseMode && (len(c.inputBuffer) > 0 || len(c.candidates) > 0) {
    if r := c.routeBufferedTriggerKey(key, &data); r != nil {
        return r
    }
}
// buffer 为空：保留原「模式键直接进模式（空）」路径
```

`routeBufferedTriggerKey` 内部按 A~F 顺序：

1. **A** 双拼韵母键 + 有 buffer → `handleAlphaKey`。
2. **B/C** 二候选键（`isSelectKey2`，`candidates≥2`）/ 三候选键（`isSelectKey3`，`candidates≥3`）→ `selectCandidate`。
3. **D** 遍历 `triggerModes()`：首个 `match` 命中的模式 → `enterModeCommitting(entry, triggerKey)`。
4. **E** 二/三候选键但候选不足（且未在 D 命中模式）→ `handleOverflowSelectKey`。
5. 返回 `nil`（→ F 标点由后续 `switch` 的 `isPunctuation` 分支处理）。

> **避免双重处理**：回落链已接管 buffer 非空时的全部 B/C/E，`switch` 中 `isSelectKey2`(`:712`)/`isSelectKey3`(`:739`) 的 case 相应简化为**仅 buffer 为空**分支（即只保留「无输入缓冲 → 按标点」逻辑，移除「有 buffer 选候选 / overflow」分支，那部分已上移到回落链）。`isShuangpinFinalKey` 的优先送引擎判断（原 `:716`/`:743`）并入回落链 A。

### 4. 通用「顶码上屏 + 进模式」
（`internal/coordinator/`，新增）

```go
func (c *Coordinator) enterModeCommitting(entry triggerModeEntry, triggerKey string) *bridge.KeyEventResult
```

逻辑（对所有模式统一）：
1. **有候选**：算高亮候选索引 `(currentPage-1)*candidatesPerPage + selectedIndex`（越界回退 0）；若该候选 `IsGroup` 或 `len(Actions)>0` → 返回 `nil`（回落标点）；否则 `finalText = doSelectCandidate(idx).Text`（继承学习 / 历史 / `recordCommit` / `confirmedSegments`）。
2. **空码**：`clearState()`，`finalText = ""`。
3. `prefix := entry.setup(triggerKey)`。
4. **有 finalText**：`resetCompositionAnchorAfterCommit()`；返回
   `InsertText{Text: finalText, HasNewComposition: true, NewComposition: isInlinePreedit()? prefix : ""}`。
5. **无 finalText**：返回 `modeCompositionResult(prefix, len(prefix))`。

> 非嵌入（非 inline preedit）模式下 `NewComposition` 必须为空，否则 preedit 字符嵌入宿主——与五笔顶码上屏（`handle_key_action.go:90-95`）同一陷阱，沿用同样处理。

## 改动点清单

| 文件 | 改动 |
| --- | --- |
| `internal/coordinator/handle_key_event.go` | 移除 `:498-510` 三段模式激活 `if`；新增 `routeBufferedTriggerKey` 统一入口并前置于二三候选处理 |
| `internal/coordinator/handle_temp_pinyin.go` | `getTempPinyinTriggerKey` 拆为 `matchTempPinyinTrigger`（纯匹配+enabled）；抽 `setupTempPinyinMode`；移除 `;`/`'` 的 `candidates==0` 内联门禁；`z` 路径保留 |
| `internal/coordinator/handle_quick_input.go` | `getQuickInputTriggerKey` → `matchQuickInputTrigger`；抽 `setupQuickInputMode` |
| `internal/coordinator/handle_temp_english.go` | `getTempEnglishTriggerKey` → `matchTempEnglishTrigger`；抽 `setupTempEnglishMode` |
| `internal/coordinator/`（新文件，如 `mode_trigger.go`） | `triggerModeEntry` / `triggerModes()` / `matchTriggerKeyInList` / `enterModeCommitting` |
| `docs/design/mode-trigger-priority-chain.md` | 本文档（含模式间优先级，供未来插入参考） |
| 相关 `AGENTS.md` | 若 coordinator 目录对外结构变化，按 CLAUDE.md 约定同步更新 |

## 不改动的部分

- 各模式既有引擎/配置门禁（临时拼音仅码表引擎等）。
- `z` 键的混输回退逻辑（`zHybridFallback`）独立保留。
- `handleOverflowSelectKey` / `handlePunctuation` 内部逻辑不动，仅调用时机被回落链编排。
- 不依赖 `punct_commit` 开关。
- buffer 为空时各模式直接进入（空）的现有路径不变。

## 测试要点

1. **二三候选优先级**：码表方案，`;` 同时配为二候选键 + 临时拼音触发键。
   - 候选 ≥ 2 → 按 `;` 选第 2 候选（不进模式）。
   - 仅 1 个候选 → 按 `;` 回落到临时拼音（顶码上屏首候选 + 进临时拼音）。
   - 无 buffer → 按 `;` 进临时拼音（空）/ 按标点（依配置）。
2. **纯模式键**：`` ` `` 配临时拼音，有候选 → 按 `` ` `` → 高亮候选上屏 + 进临时拼音，`` ` `` 不上屏；移动高亮后上屏的是高亮候选。
3. **空码**：无效编码 → 按模式键 → 编码丢弃，直接进模式，无中文上屏。
4. **多模式同键优先级**：同一键配给快捷输入 + 临时拼音 → 命中**快捷输入**（顺序在前）。
5. **未来模式插入点**：验证 `triggerModes()` 顺序为 快捷输入 → 临时拼音 →（占位）→ 临时英文。
6. **边界候选**：高亮为 cmdbar 命令 / 组候选 → 按模式键 → 回落标点，不进模式、不触发命令。
7. **回落到符号**：键不是任何角色 → `handlePunctuation` 正常。
8. **快捷输入 / 临时英文** 与临时拼音同样支持「顶码上屏 + 进模式」。
9. `Shift+` 触发键 → 不进模式（沿用 `!hasShift`）。
10. 非码表引擎 → 临时拼音不触发；其它模式按各自引擎门禁。
11. inline / 非 inline preedit 两模式：上屏后候选窗定位正确、触发键字符不嵌入宿主。
12. 顶码上屏候选被正确学习（自动造词不漏字）、计入输入历史。
