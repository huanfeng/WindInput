# 按键管线 / 模式融合 / 符号流水线 / 全码策略 / redb 接线 —— 系统性重设计

> ⚠️ 本文的 Processor / Capability 目标架构已按 `wind-coordinator/src/pipeline.rs` 裁剪，未照此实现。
> 现行的阶段划分与功能落点见 [architecture/feature-placement-guide.md](../architecture/feature-placement-guide.md)。

> 权威设计差分。证据取自 Go 版（仓库 `../WindInput`，引用 `file:line`）与 Rust 现状
> （`wind_input/crates/wind-coordinator`）。配套已有：[engine.md] [dict.md] [store.md]
> [coordinator.md] [config-schema.md] [frequency.md]。本篇聚焦**交互管线的架构统一**，
> 是把零散模式标志、散落符号判定、基于 scheme 的全码逻辑，收敛为单点决策的目标态。

## 0. 为什么要重构（问题陈述）

Rust 现状（`coordinator.rs`）：

- **模式是散装 bool 标志**：`temp_pinyin_mode / quick_input_mode / temp_english_mode` 各自
  flag + buffer + 散落的 `handle_*` 分支；**无** special / url 模式；**无**统一决策器。
  新增/改一个模式要改多处入口，回归源就在此。
- **符号判定散落**在 `handle_key_event` 各处；无单一优先级链。
- **全码/上屏逻辑基于 scheme**（引擎层 per-scheme 配置）。但临时英文/临时拼音/URL/快捷输入/
  特殊模式下当前激活的不是普通 scheme，**"该用哪套全码/上屏配置"无明确归属** → 歧义。
- **redb 未接线**：coordinator 仍用 legacy `FreqTracker`（存文件）+ `ShadowStore`（内存），
  **无用户造词写路径**；redb 后端（`wind-store` 全套）建好但闲置。

## 1. Go 参照架构（证据）

### 1.1 决策器 = 模式融合的核心（`pipeline_decider.go`）

Go 已退役"散装分支"，决策器 `decider` 成**唯一键事件路径**（commit `a477b830`）：

```
type decider struct{ tempPinyin, quickInput, tempEnglish, special, url Processor; ... }   // :12
decide(key, data) → (KeyEventResult, handled)                                              // :132
  ├ keyHandlerChain() 有序链                                                                // :122
  ├ tryActivateSpecial / tryActivateFromEmpty （空缓冲激活：quick>temp_pinyin>temp_english）// :172/:183
  ├ executeActivate(p, decision)                                                            // :199
  ├ dispatchHostChain(key, data)  （已激活 processor 全接管该键）                            // :220
  ├ armRewind / canRewind / rewindHijack  （统一夺取回退：URL、z 回退共用）                  // :244/:261/:268
  └ applyEngineDiff(needed Capability)  （切 processor 时引擎层副作用单点）                   // :297
```

`HandleKeyEvent` 顶层优先级链（`handle_key_event.go`）：全局热键(:113) → 加词模式(:172) →
模式切换键(:202) → Ctrl/Alt 透传(:309) → 英文模式(:343) → 受管宿主模式内键(:523
`dispatchHostChain`) → URL 前缀夺取(:529) → 输入态回落链(:540 `routeBufferedTriggerKey`) →
空缓冲触发激活(:549) → Shift+字母(:563) → **engine_default 兜底**(:591 `dispatchHostChain`)。

关键提交时间线：`fc185018`(决策器默认开) → `a477b830`(退役 decider-off 成唯一路径) →
`18aeca7b`(engine_default 进决策器链) → `0115ad58`(统一夺取回退 + URL) →
`d593742a`(z 键回退迁入统一机制) → `c3c79d7b`(applyEngineDiff 单点)。

### 1.2 配置随 Processor 走（解决全码歧义的关键）

Go 中**各特殊模式自带引擎实例 + 自身配置，无 fallback**：

| 模式 | 引擎/配置来源 | 全码/上屏策略 | 证据 |
|---|---|---|---|
| 正常码表 | 主 scheme 码表引擎 | `AutoCommitAtFull` / `MinAutoCommitLen` / `ClearOnEmptyAt4` | `codetable.go:34/37` |
| 临时拼音 | **独立**拼音引擎实例 | 拼音 schema 自身配置 | `manager_temp_pinyin.go:145` |
| 特殊模式 | **自身**码表实例 | `AutoCommit: prefix_free/fixed_length/manual` + `FixedLength` | `handle_special_mode.go:148`, `config.go:122` |
| URL | **无引擎**（前缀夺取） | 无全码；空格/回车上屏原文 | `handle_url.go:21` |
| 临时英文 | 英文词库查询 | 无全码（无码长） | `handle_temp_english.go:84` |
| 快捷输入 | 计算/日期/查询 | 无全码 | `handle_quick_input.go:135` |

> **结论**：全码/空码策略不是全局 scheme 属性，而是**当前激活 Processor 的属性**。
> "命令直通车"（命令候选 `IsCommand` + Actions，`handle_candidates.go:1061`）走候选副作用通道，
> 不参与造词学习，也不归任何 scheme 全码逻辑。

### 1.3 符号/标点优先级链（`handle_punctuation.go` / `handle_smart_symbol.go`）

```
按键为标点字符 r：
  1. 智能符号模式：同键连按 & 武装 & 时限内 & prevChar 匹配 → ReplaceBackward 转英文  (smart_symbol :39)
  2. 自动配对：左标点输出 + 压栈 + 光标回退                                            (auto_pair)
  3. convertPunct(r, afterDigit, prevChar):
       a. 自定义映射 LookupCustom 命中 → 直接返回（4 态：中半/英全/中全/英半）          (:911, transform/punctuation.go:104)
       b. 数字后智能转换：SmartPunctAfterDigit && r∈SmartPunctList && prevChar∈0-9 → 英文标点 (:361)
       c. 中文标点转换 ToChinesePunctStr（引号左右交替状态机）
       d. 全角转换 ToFullWidth
```

配置项（`config.go`）：`SmartPunctAfterDigit`(默认 true)、`SmartPunctList`(".,:")、`SmartSymbolMode`
(默认 false)、`SmartSymbolTimeoutMs`(500)、`SmartSymbolChars`、`PunctCustom{Enabled,Mappings}`、
`AutoPair{Chinese,English,Blacklist,Pairs}`、`PunctFollowMode`。转换器为**共享单例**，模式切换/激活
时 `Reset()` 引号状态（commit `5c8432d0` 智能符号、`85243c17` 数字后标点可配）。

## 2. 目标架构（Rust）

### 2.1 Processor trait + Decider（模式融合）

新建 `wind-coordinator/src/pipeline/`（mod）：

```rust
/// 一个输入宿主/模式。正常码表/拼音输入也是一个 default processor。
trait Processor: Send + Sync {
    fn id(&self) -> ProcId;                       // Default | TempPinyin | TempEnglish | QuickInput | Special(u8) | Url
    fn capability(&self) -> Capability;           // 需要的引擎层（见 2.2）
    fn commit_strategy(&self) -> CommitStrategy;   // 全码/空码策略归属（见 2.3）
    /// 空缓冲时是否夺取激活（按触发键）。返回激活决策（含是否先顶码上屏）。
    fn try_activate(&self, cx: &mut Ctx, key: &KeyIn) -> Option<Activation>;
    /// 已激活时处理键。返回 Outcome（候选更新/上屏/透传/退出）。
    fn handle_key(&self, cx: &mut Ctx, key: &KeyIn) -> Outcome;
    fn on_enter(&self, cx: &mut Ctx);
    fn on_exit(&self, cx: &mut Ctx);
}

struct Decider { default: Box<dyn Processor>, hosts: Vec<Box<dyn Processor>>, active: Option<ProcId>, rewind: Option<Rewind>, ... }
impl Decider { fn decide(&mut self, cx, key) -> KeyAction { /* 见下序 */ } }
```

`Decider::decide` 顺序（对齐 Go，去掉 Go 历史包袱）：

1. 模式切换键（Shift/Ctrl/Caps）→ `take_input_on_mode_switch`（见已实现）。
2. Ctrl/Alt 非热键 → 清空+`notify_ui_hide`+透传（已修）。
3. `active` 有宿主 → `host.handle_key`（dispatch_host_chain），含退出判定。
4. 统一夺取回退：`rewind_armed && 退格到前缀边界` → `rewind_hijack`。
5. 输入态回落链（有 buffer/候选）：二三候选键 > 模式激活(顶码上屏+进模式) > overflow > 落 default。
6. 空缓冲触发激活：优先级 quick > temp_pinyin > special > temp_english；URL 前缀夺取。
7. default processor（码表/拼音正常输入）。

> **现有 `temp_pinyin/quick_input/temp_english` 收编为 Processor 实现**，删散装 flag 的分散入口；
> State 仍可保留 buffer 字段，但**入口唯一**（Decider）。

### 2.2 Capability —— 引擎副作用单点（对齐 `applyEngineDiff`）

> **实施修正（S1 勘探代码后裁掉，2026-06-17）**：Capability 在 Rust **不移植**。Go 需要
> `applyEngineDiff` 是因为它在切 processor 时挂卸**共享引擎**的词典层；而 Rust 各模式按
> schema id **独立查询**引擎（`EngineManager::convert_with(schema_id, ...)`），不存在被多模式
> 改写的共享引擎，故没有「引擎副作用」需要单点统一。强行引入只是死抽象。原设计如下存档：

```rust
bitflags Capability { CODETABLE, PINYIN, ENGLISH_DICT, NONE }
```

切换 `active` processor 时，`Decider` 比较 `capability()` 差分，调用 `DictManager` 单点挂卸层
（临时拼音挂拼音层/卸码表层等）。**唯一**改引擎层状态的地方，杜绝多处副作用漂移。

### 2.3 CommitStrategy —— 配置随 Processor 走（根治歧义）

> **实施说明**：推迟到 **S4** 落地。Rust coordinator 当前**尚未实现**全码/空码上屏
> （`codetable::engine::should_auto_commit` 为空实现），现在没有调用点消费策略归属，
> 故按「避免过早抽象」与实现 S4 时一起设计。设计目标态如下：

```rust
enum CommitStrategy {
    SchemeCodeTable,          // 用主 scheme 码表配置（AutoCommitAtFull/MinAutoCommitLen/ClearOnEmpty）
    PinyinEngine,             // 用（独立）拼音引擎配置
    Special { auto: SpecialAuto, fixed_len: usize },  // prefix_free | fixed_length | manual
    None,                     // URL/临时英文/快捷输入：无全码概念
}
```

"当前全码/上屏策略" = `active_processor.commit_strategy()`，**不再读全局 scheme**。这把"全码逻辑
基于 scheme 造成特殊模式歧义"从根上消除：策略归属随激活模式，命令直通车走候选副作用通道（不进全码）。

### 2.4 统一夺取回退（`armRewind/rewindHijack`）

进入"夺取式"模式（URL 前缀夺取、z 触发临时拼音）时登记 `Rewind{snapshot, host_text, cleanup}`；
在边界退格时撤销夺取、回放快照到正常输入流。URL 与 z 共用一套，避免各写各的回退。

### 2.5 符号/标点单点流水线

新建 `resolve_punct(cx, ch) -> PunctOutcome`，优先级严格如 §1.3：
智能符号(连按) → 自动配对 → 自定义映射 → 数字后智能 → 中英标点(引号状态机) → 全半角。
`PunctConverter` 为 coordinator 持有的单例，模式切换/激活 `reset()` 引号交替状态。
**所有标点输出走此一处**，删散落判定。

### 2.6 redb 接线（S0，独立先行）

- coordinator 实例化 redb `Store`（路径用 `Config::local_dir`，debug 变体隔离已就绪）。
- `FreqTracker`(文件) → redb `FREQ` 表（`record_freq/get_freq`，衰减见 [frequency.md]）。
- `ShadowStore`(内存) → redb `SHADOW` 表（`pin/delete/remove/get_rules`）。
- 候选选中写路径：`on_word_selected`（user_words count/last_used）+ 临时词 `learn_temp_word`。
- `DictManager` 注册 `StoreUserLayer/StoreTempLayer`，用户词/临时词进候选合并。
- 失焦 `save`（已有落盘时机）改为 redb 事务提交（redb 本身事务持久，`save_freq` 文件路径退役）。

## 3. 新功能挂载点

| 功能 | 挂载 | 配置（schema 级优先 > 全局 > 默认，见 config-schema.md） |
|---|---|---|
| 数字后智能转换 | §2.5 流水线 b 步 | `input.smart_punct_after_digit` + `smart_punct_list` |
| 智能符号模式 | §2.5 流水线 1 步 | `input.smart_symbol{enabled,timeout_ms,chars}` |
| 自定义标点映射 | §2.5 流水线自定义映射 | `input.punct_custom{enabled,mappings: map<str,[4]>}` |
| 临时英文 | TempEnglish Processor | `input.shift_temp_english{enabled,shift_behavior,trigger_keys,allow_symbols,space_as_input}` |
| 网址模式 | Url Processor + 夺取回退 | `input.url_input{enabled,prefixes[]}` |
| 特殊模式 | Special Processor（自带码表/策略） | `features.special_modes[]{id,table,trigger_keys,auto_commit,fixed_length}` |
| 按钮自定义 | 热键/候选操作键映射 | `hotkeys.*` + 候选操作键组 |

## 4. 分阶段实施（每阶段：IME 可用 + 测试 + 交叉编译 + 提交）

- **S0 redb 接线**（§2.6）：独立地基，落实用户造词/词频持久化。✅ 完成。
- **S1 模式收编为单点决策**（§2.1）：三散装 bool → 单一 `State.active: Option<ModeKind>` + 单点 `match` 分派 + 单一激活/复位入口。✅ 完成（commit `432cbc8`）。
  - 修正：Capability 不移植（§2.2，Rust 无共享引擎副作用）；CommitStrategy 推迟到 S4（全码上屏尚未实现，当前无消费方，避免过早抽象）。enum 分派替代 trait 对象（模式逻辑与 Coordinator 紧耦合，trait+Ctx 间接层不划算）。
- **S2 符号单点流水线**（§2.5）：自定义映射 + 智能符号 + 数字后智能 + 全半角。✅ 完成。
  - S2a（commit `09a00bf`）：统一 `convert_punct(state,ch,prev_char)`（自定义映射>数字后智能>中文标点>全半角）；config 增 `punct_custom`/`smart_symbol*`；transform 增 `lookup_custom`/`peek_chinese_str`/`revert_last_quote`。
  - S2b（commit `48ec31d`）：智能符号模式 + 协议 `CMD_REPLACE_BACKWARD=0x0109`/`KeyAction::ReplaceBackward`；`try_smart_symbol_replace` 在标点分支入口短路；焦点/模式切换 disarm。
- **S3 新模式**（§2.4）：URL + 特殊模式接入 Decider；统一夺取回退。✅ 完成。
  - S3a（commit `d940f33`）：网址模式。config 增 `input.url_input{enabled,prefixes}`；`ModeKind::Url`；普通输入累积到 `input_buffer+键==前缀` 时夺取；`handle_url_key`（原样累积/空格回车上屏原文/退格删空退出/Esc 放弃）；`printable_char` VK→ASCII。
  - S3b（commit `72b6602`）：统一夺取回退。`pipeline::Rewind{snapshot,host_text}`；进入 URL 时登记，退到前缀边界再退格→`rewind_hijack` 回放快照到正常码表输入流。`active_hijack_buffer/can_rewind/rewind_hijack` 模式无关。
  - S3c（commit `0302f0e`）：特殊模式。config 增 `features.special_modes[]`；`ModeKind::Special(u8)`；`CodetableDict` 懒加载+缓存（[用户/data]/schemas 查找）；`decide_special_auto_commit` 纯函数（prefix_free/fixed_length/manual）；`handle_special_key` 全套。
  - 修正：**z 键回退**（z+字母无前缀→临时拼音）未随 S3 落地——它是独立的临拼补全特性（需把 z 配为临拼触发键，默认 backtick 不触发），且依赖「加键后无码表前缀」的引擎前缀探测。S3b 的 Rewind 机制已做成模式无关，z 落地时（并入临拼补全或 S4）可直接接入 `active_hijack_buffer` 的 `TempPinyin` 臂。
- **S4 全码/空码策略落地 + 模式激活键融合**（§2.3）：✅ 完成。
  - S4a（commit `2403194`）：码表全码自动上屏。`CodeTableEngine` 注入 `auto_commit_at_full`/`auto_commit_min_len`；`decide_auto_commit` 纯判定（唯一精确 + 无更长后继）填 `ConvertResult.should_commit/commit_text`；coordinator 字母分支消费、shadow 后复核存活才上屏。6 单测。
  - S4c（commit `3ecf217`）：混输全码自动上屏 + 拼音守护。`MixedEngine` 取主码表意向，`auto_commit_block_on_pinyin`（默认 true）存在拼音候选时否决。3 单测。
  - S4b（commit `fce9abb`）：空码清空 + 顶码上屏。`ConvertResult.should_clear`（满码无候选 + `clear_on_empty_max` + 无更长后继）；`handle_top_code` 上移到 `Engine` trait（默认 None），码表实现「超满码长 + 整串无匹配 + 无更长后继 → 顶前 N 码首选、余码续打」，`MixedEngine` 委托主码表，`EngineManager::handle_top_code` 暴露；coordinator 引入 `InputOutcome{Normal,AutoCommit,Clear}` 统一字母分支结局，顶码先于候选刷新。`CommitOptions` 结构统一码表上屏开关。
  - **想法一·全局默认 + 方案可选覆盖**（commit `fce9abb`）：config 增 `[input.code_commit]` 全局默认；方案级 `clear_on_empty_max`/`top_code_commit` 升为 `Option<bool>`（tri-state）；解析 = 方案 Some > 全局 > 内置默认（`at_full` 额外兼容 legacy `auto_commit_unique`）。全局配置在 `config.toml`（用户可合并）→ **无需改只读安装目录 schema 即可调全码策略**，顺带解决了"非主方案统一控制"。
  - S4d（commit 见下）：模式激活键融合。散落的临时英文/快捷输入/临时拼音/特殊模式四处激活 `if` 块收敛为单一 `try_activate_mode(state, data) -> Option<KeyAction>`，优先级链显式（临英 > 快捷 > 临拼 > 特殊），行为不变。URL 前缀夺取为「缓冲扩展夺取」语义，不在此链，单独保留。
  - 实施说明：决策落在引擎层（host 可测）而非 coordinator，因引擎持 `dm` 可判 `has_longer_code`；CommitStrategy enum 未显式建模——策略已随激活引擎（码表/混输各自实现 `should_auto_commit`/`handle_top_code`），无独立消费方，避免过早抽象。
  - **后续大阶段（用户拍板）**：把临时拼音/特殊模式的引擎统一为 scheme 实例（base 持久 / overlay 瞬态分离的 Processor-lite），再支持临时 mix 复合引擎融合临拼/快符/生僻字。见对话决议。
- **S5 按钮自定义**。

## 5. 配置 schema 增补（详见 config-schema.md 收口）

`input.{smart_punct_after_digit, smart_punct_list, smart_symbol{...}, punct_custom{...},
url_input{...}, shift_temp_english{...}}`、`features.special_modes[]`、`hotkeys.*`。
schema 级可覆盖全局；缺省回退全局再回退内置默认（三层合并已实现）。

---

### 决策小结（供评审）

1. **决策器单点**：模式融合靠 `Decider + Processor`，删散装 flag 入口。
2. **配置随 Processor 走**（`CommitStrategy` + `Capability`）：根治"特殊模式/直通车用哪套全码配置"。
3. **符号单点流水线**：固定优先级，删散落判定。
4. **redb 先行**（S0，独立）：用户词/词频真正落库。
5. 不保留 legacy 双轨（file freq / 内存 shadow / 散装模式），一步到目标态。
