# 功能落点指引：输入流程的阶段划分与挂载点

> 新功能动手前，先按 §3 的清单走一遍，确定它落在哪些阶段、挂在哪个点、要同步哪些副本路径。
> 设计文档 / PR 描述里写一行「落点：P2 + P6（K18、出口收尾）」，审查时据此核对。
>
> 本文是**索引**：阶段边界、挂载点、副本路径清单、组件化方向在这里；各阶段的算法细节
> 只在它的唯一出处（引擎与候选 → [engine-candidate-pipeline.md](engine-candidate-pipeline.md)；
> 跨组件硬约定 → 根 [AGENTS.md](../../AGENTS.md)「跨组件硬约定」；协调器字段归属 →
> [coordinator-state-inventory.md](../design/coordinator-state-inventory.md)），这里只引用不复述。

## 1. 为什么不是 Rime 式管线

本仓刻意**不**做可插拔的 Processor / Segmentor / Translator / Filter 组件链（决定与理由见
`wind-coordinator/src/pipeline.rs` 模块文档）：各模式按方案 id 独立查引擎，没有被多模式改写的
共享引擎；按键处理与协调器状态紧耦合，套 trait 间接层只会更难读。

但「不做插件」不等于「所有功能都写成散落的 if」。本仓的取舍是：

- **顺序即契约的地方**（按键优先级链、模式分派）保持显式代码，用编号标注槽位（§4.1），
  新增分支必须说清「它必须先于谁、晚于谁」。
- **同一道加工被多条路径各抄一份的地方**（候选后处理、上屏出口收尾）收成「一份有序定义 +
  各路径声明用哪几步」（§5）。散落 if 的真正病灶在这里，不在缺框架。

## 2. 阶段划分

```
宿主按键
  │
  P0 准入 ── 这个键归不归输入法（C++ OnTestKeyDown / key_gate）
  │
  handle_key_event_policed ─ 前置（智能符号失效、补空格锁存）
  │  └ handle_key_event
  │      P1 归一与拦截 ── K1–K15
  │      P2 模式分派与激活 ── K16–K25
  │          │ 缓冲变了
  │          └→ P3 候选生成 → P4 候选后处理 → 候选窗
  │          │ 选词 / 顶屏
  │          └→ P5 上屏
  └ 后置：P6 出口收尾（补空格、换行、造词投喂、武装失效、联想占位…）
```

| 段 | 职责 | 落点 | 归入本段的判据 | 本段不变量 |
|---|---|---|---|---|
| **P0 准入** | 这个键该不该由输入法处理 | `key_gate.rs::should_handle_key`（Android 已用）；C++ `OnTestKeyDown`（尚未迁移，是副本） | 功能让一个原本透传的键被吃，或反之 | C++ 吃键集 ⊆ Rust 出字集；判定只读不改状态 |
| **P1 归一与拦截** | 某种状态下某键另有含义，不拥有缓冲 | `handle_key_event` 槽位 K1–K15 | 「候选窗显示时按 X 做 Y」「加词窗里 ↑↓ 调词长」这类 | 守卫写在函数内，不成立返回 `None` 原样落回；先后顺序有理由并写在注释里 |
| **P2 模式分派与激活** | 有自己的缓冲、自己解释按键序列的会话态 | K16 `match state.active` → `handle_*_key`；K18–K19、K24 激活与夺取；`pipeline.rs` 的 `ModeKind` / `Rewind` | 功能有独立缓冲或独立的按键语义 | 同时至多一个独占模式；夺取必须可回退；新模式是 `ModeKind` 变体，不是 bool |
| **P3 候选生成** | 产生候选：引擎查询、短语注入、基础排序、去重 | `Engine::convert`（wind-engine）；`build_candidates` 前半段；各模式的 `refill_*` / `update_*_candidates` | 新增候选来源，或改召回与切分 | 排序落到 weight；音节切分留在引擎内；去重要并码位（`absorb_codes_from`） |
| **P4 候选后处理** | 对已有候选删、移、插 | `build_candidates` 后半段及 §4.2 的平行路径 | 不产生新来源，只改已有列表 | 调频不改 weight；shadow 是用户的最终话语权；所有池子走同一条链（§4.2） |
| **P5 上屏** | 候选变成文本：记账、简繁、英文词后空格、清会话 | `commit_candidate`；文本变换唯一出口 `maybe_convert`（handle_mode.rs） | 变换依赖候选身份（来源、码、词频文本） | 记账用 `freq_text` 不用显示文本；简繁只经 `maybe_convert` |
| **P6 出口收尾** | 凡上屏或凡组合结束都要做的事 | 平台中立的进 `session_exit.rs::finish_session_action`；键位相关的进 `handle_key_event_policed` 后置段 | 与具体候选无关，或依赖本键最终的 `KeyAction` | 只挂出口，不散到 40 多个返回点；会话层判定先于显示层加工 |

不算流程段、但各段都要用的三个横切面：配置快照（`self.rt()` / `ConfigBundle`、
`key_resolver.rs`）、输出通道（`session_output.rs` 的 `emit_*`）、平台能力（`host_services.rs`）。

## 3. 落点判定清单

按顺序问，可多段命中。每一问都给出挂载点与**必须同步的副本**——漏同步是本仓「改了一处、
另一条路没生效」类缺陷的主要来源。

1. **它让某个键的吃 / 放发生变化吗？** → P0。改 `key_gate.rs` 并同步 C++ `OnTestKeyDown`；
   先证明 Rust 在同条件下必定出字（AGENTS.md 硬约定），跑 `wind_tsf/tests`。
2. **它有自己的缓冲、或自己解释按键序列吗？** → P2。加 `ModeKind` 变体 + K16 一条分支 +
   `handle_*_key`；然后逐个过所有以 `state.active` 为键的 `match`（分派、翻页扩容
   `expand_candidates`、鼠标点选 `select_candidate_at`、数据归属 `effective_data_schema`…）。
   这些 `match` 不许有兜底臂（守卫见 §5.4 第 5 步），加了变体后编译器会逐处报出来；
   `matches!` 与 `if let` 不在其内，要另行 grep `ModeKind::`。
   前缀触发、需要能退回来的，复用 `try_prefix_hijack` + `Rewind`，不另写回退。
3. **只是某态下某键多一个含义？** → P1。写成 `try_*(&self, data) -> Option<KeyAction>`，
   按「它必须先于谁」选槽位（§4.1）。若这个键值只对某个动词有意义，按
   [key-resolver-unification.md](../design/key-resolver-unification.md) §5 做成该动词的参数，
   不新增绑定、不做通用降级链。键位可配置时，出厂值落在用户清得掉的字段上（AGENTS.md）。
4. **它产生新的候选来源吗？** → P3。只跟某方案相关的放进引擎；跨引擎通用的在协调器注入
   （参照短语注入），并正确设置 `source` / `is_prefix` / `is_partial`——否则 P4 的分层排序与
   调频会把它排到错位。
5. **它对已有候选删、移、插吗？** → P4。先定它相对调频与 shadow 的位置；再对 §4.2 清单的
   **每一条**路径回答「接」或「不接及理由」。自己另解码一份候选池的，同样要走这条链。
6. **它在上屏时改文本吗？** 依赖候选身份 → P5（`commit_candidate` / `maybe_convert`）；
   不依赖 → P6。
7. **它在上屏后或组合结束后有副作用吗？** → P6。平台中立的加进 `finish_session_action`，
   在 `ExitSteps` 加一个字段，`exit_steps` 里逐个来源填「接」或「不接」并写理由；只与按键
   相关的才挂 `handle_key_event_policed`。
8. **它涉及平台差异吗？** 平台能力走 `host_services.rs`，输出走 `session_output.rs`；
   跑 `cargo check-headless` / `check-android` 与 `tests/output_seam.rs`。
9. **它引入新状态吗？** 按 coordinator-state-inventory.md §5 判归属，并写明「谁在什么时候清它」。
10. **它在输入流程里需要配置、方案或数据文件吗？** → 必须满足 §6（零读盘）。

**收尾义务**（与段无关）：改了引擎 / 候选逻辑同步 engine-candidate-pipeline.md；用户能感知到候选
有无、先后、条数或上屏时机变化的，同分支登记
[candidate-behavior-changelog.md](candidate-behavior-changelog.md)；新增槽位、平行路径或出口时
更新本文 §4。

### 3.1 用已有功能反向验证

| 功能 | 命中 | 落点 |
|---|---|---|
| 短语注入 | 问 4 | P3，`build_candidates` 内注入，按自身 weight 与码表竞争 |
| 直接辅助码 | 问 5 | P4，钉在全部重排之后、shadow 之前；`decode_direct_prefix` 另解码前缀池，正是问 5「自己另解码一份也要走链」那一条 |
| 中英补空格 | 问 6 否 → 问 7 | P6：入口锁存光标前一字、`finish_session_action` 追加空格，四个来源共用 |
| 智能符号 | 问 1、2、7 | P0（透传位 `TOGGLE_PASSTHROUGH_KEY`）+ P2 默认臂的标点处理 + P6 出口的武装失效；移动端不经出口，另有判据兜底 |

## 4. 各阶段挂载点

### 4.1 按键链槽位（`coordinator/message_handler.rs::handle_key_event`）

代码里以 `// 〔Kn …〕` 标注。新增分支时插在两个槽位之间，在注释里写明先后理由；需要新槽位时
在这里和代码里同时编号（可用 `K8a` 这类后缀，避免整体重编）。

| 槽位 | 内容 | 段 |
|---|---|---|
| K1 | 修饰键原始上报（Ctrl 单击判定） | P1 |
| K2 | 组字中 Alt：注释态 | P1 |
| K3 | 注释功能键按住 | P1 |
| K4 | 每键复位与观测：统计 / 会话标志复位、方案行为同步、小键盘归一、打字节奏、CapsLock 校准（不返回） | P1 |
| K5 | key_up：切换键与修饰键选词 | P1 |
| K6 | 右键菜单接键 | P1 |
| K7 | key_down 全局热键 | P1 |
| K8 | 候选期快捷键组：`try_quick_input_select` / `handle_candidate_action_hotkey` / `try_alt_commit` / `try_english_case_cycle_key` / `try_sentence_cycle_key` | P1 |
| K9 | 快捷加词模式 | P1 |
| K10 | 密码框透传 | P1 |
| K11 | 软键盘 | P1 |
| K12 | 配对跳出 | P1 |
| K13 | 英文模式分水岭（全角 / 英半标点 / 透传） | P1 |
| K14 | CapsLock 大写 | P1 |
| K15 | 夺取回退（`Rewind`） | P2 |
| K16 | 独占模式分派 `match state.active` | P2 |
| K17 | 方案级锁外状态切换 | P2 |
| K18 | 空缓冲模式激活 `try_activate_mode` | P2 |
| K19 | 前缀夺取 `try_prefix_hijack` | P2 |
| K20 | 编码字符闸门 / 符号缓冲闸门 | P2 |
| K21 | 以词定字 | P2 |
| K22 | 会话动作（导航 / 翻页 / 取消…，`apply_session_action`） | P2 |
| K23 | 小键盘 direct | P2 |
| K24 | z-fallback 夺取 | P2 |
| K25 | 默认按键臂（Esc / 退格 / 空格 / 回车 / 字母 / 数字 / 标点） | P2 |

K8 那五个 `if let Some` 已经是一张有序表，不再抽成函数指针数组——调用点只有一个，改了也不减少分支。

### 4.2 候选后处理的平行路径（现状）

规范顺序：**过滤 `apply_filter` → 单字 `apply_single_char` → 调频 `apply_freq_rerank*` → shadow
`apply_shadow*`**。主链在调频与 shadow 之间另有英文定位、整句块、纠错护顶、直接辅助码，之后有
空码补全收口、出简让全、emoji 扩展、自动上屏复核（细节见 engine-candidate-pipeline.md §8）。

各路径都经步骤表（`wind-coordinator/src/postprocess.rs`）执行，接哪几步以 profile 常量为准：

| 路径 | 函数 | profile |
|---|---|---|
| 主链 | `build_candidates` | `MAIN_PRE`（过滤、单字、调频）+ 收尾 `MAIN_SHADOW` |
| 整句池 | 同上，`sentence_pool` | `SENTENCE_POOL`（过滤、单字、shadow） |
| 空码补全池 | 同上，`completion_pool` | `COMPLETION_POOL`（过滤、单字、shadow） |
| 直接辅助码前缀池 | `handle_direct_aux.rs::decode_direct_prefix` / `merge_direct_hits_into` | `DIRECT_AUX_PREFIX`（过滤、单字、调频）+ 合并前 `DIRECT_AUX_MERGE` |
| 临时拼音 | `handle_temp.rs::refill_temp_pinyin_candidates` | `TEMP_PINYIN_PRE`（过滤、调频）+ 纠错护顶后 `TEMP_PINYIN_SHADOW` |
| 临时英文 | `handle_temp.rs::refill_temp_english_candidates` | `TEMP_ENGLISH`（调频、shadow） |
| mix 自由英文段 | `handle_mode.rs::mix_free_english_segment` | `MIX_ENGLISH_SEGMENT`（调频、shadow） |
| 特殊 / 生僻字模式 | `handle_special.rs::update_special_candidates` | `SPECIAL_PRE`（调频）或生僻字 `RARE_CHAR_PRE`（空）+ `SPECIAL_SHADOW` |
| 候选反查 | `handle_reverse.rs::build_reverse_candidates` | `REVERSE`（过滤） |

留在表外的两处，调频都用 `apply_freq_rerank_with_extra`（要并入快捷输入历史的外部热度记录，步骤表的
`FreqRerank` 不带这个输入）：

- mix 成员段（`handle_mode.rs::refill_mix_candidates`）：调频与 shadow 之间还夹着纠错护顶，shadow
  直接调 `apply_shadow_in`；
- 快捷输入历史并进英文段（`quick_history.rs::mix_free_with_history`）。

不为这两处扩 `PassCtx`。

**未定项**：临时拼音从未接单字（`SingleChar`）。第 3 步按纯重构保持现状，是否该接待定。
已排除的方案：跟随主方案的单字开关——临拼是码表方案用户临时打拼音用的，拼音方案用户并不用它，
主方案的开关表达不了临拼用户的意图（2026-11-13 维护者定）。

翻页扩容（`expand_candidates`）经 `build_candidates_at_cursor` 回到主链整链重建，不是独立路径。

守卫（`handle_candidate.rs` 的源码扫描测试；生产代码统一经 `source_scan::production_code` 取得，
它按 rustfmt 缩进剔除每个 `#[cfg(test)]` 条目，真实文件上的覆盖由探针测试钉住）：

- `every_shadow_read_goes_through_normalized_code`：`apply_shadow*` 的实参与 `PassCtx { shadow_code }`
  字段都须取归一后的码；
- `schema_scoped_reads_writes_and_debug_share_one_source`：读写调试三端与 `PassCtx { owner }`
  字段都须取自登记过的归属来源；
- `postprocess.rs` 内：每个 profile 是规范顺序的子序列；主链单字先于 shadow；两个池接过滤、单字、
  shadow；两个池的调用点用的是各自的 profile。

emoji 不在步骤表内，「emoji 各路都接」仍是独立扫描。

### 4.3 上屏出口（现状）

各上屏来源的平台中立收尾统一经 `session_exit.rs::finish_session_action`，接哪几步看同文件的
`exit_steps` 表（按来源穷尽 `match`，新来源必须填一行；源码扫描守卫禁止绕过它直接调各步）。
直接推文本的通路经 `push_commit_text_from(origin, text)` 进来。补空格与产出动作同在一把状态锁内，
其余各步放锁后各自取锁。

| 步骤 | 按键 | 鼠标点选 | 移动端选词 | CapsLock 钩子 | 悬停提示 | 命令输出 | 软键盘点击 |
|---|---|---|---|---|---|---|---|
| 中英补空格 | ✓ | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ |
| 换行改写 | ✓ | 推送编码时 | 同左 | 同左 | 同左 | ✓ | ✓ |
| 输入统计兜底 | ✓ | ✗ | ✗ | ✗ | ✗ | ✗ | ✓ |
| 自提交打点 | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| 草稿滑窗 | 喂入 | 喂入 | 不碰 | 喂入 | 打断 | 打断 | 喂入 |
| 检索放宽复原 | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| 配对保活 | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| 收掉无占位联想 | 分派处理 | ✗ | ✗ | ✗ | ✓ | ✓ | ✓ |

- **命令输出**：`$CC` 命令的 `type()`、工具栏按钮 / `command:` 绑定、mac / Linux 粘贴短语、`ime.pair`
  配对符号。不是输入：打断滑窗（同时标词边界），不补空格（格式由命令自定），不记统计。命令栏连续多段
  `type()` 的撤销计数累加成一笔，中间执行过副作用动作（`key.tap` 等）则复位为 1。
- **收掉无占位联想**：插进来的不是联想的下文。Windows 上此前靠光标回声被当成「用户挪光标」顺带收掉，
  打了自提交点后回声被忽略，要在出口显式收。
- **软键盘点击**与物理键敲软键盘（按键来源）对齐。
- **移动端选词不碰滑窗**：移动端按键上屏不经本出口、不喂滑窗，只喂选词一侧会把不相邻的两次上屏拼成假词。
- **统计兜底**只给按键与软键盘点击：候选类来源的上屏出口自己 `record_commit`，兜底只补漏记的 InsertText，
  收益小（2026-11-13 定）。非按键来源接兜底时在出口复位去重标志 `stat_recorded`。
- **撤销计数**只计光标前的文字：带光标定位的插入（配对「（|）」）按回退格数（字符）去掉右段；
  换行改写在打点之前，计数按改写后的文本。

键位相关、只属于按键路径的（不进表）：入口处的透传位解除智能符号武装与补空格锁存；出口处的
上屏码重新分派（CapsLock 钩子也做）、hold 清理、非同键解除武装（鼠标点选在选词前另行解除）、
联想占位两道收口、交还宿主重放的标记、无占位联想会话位对账、组合区占位 / 音节分隔符（推送路径在
`push_no_key_ctx_action` 里换分隔符）。

移动端按键直调内层 `handle_key_event`，不经上述任何出口，不补中英交界空格（`wind-mobile` 只补
英文词后空格）；移动端候选栏选词（`candidate_pull::select_candidate`）是 `MobilePick` 来源。

不在来源表内的两条：

| 通路 | 位置 | 现状与理由 |
|---|---|---|
| 切方案 / 中英切换时上屏 | `coordinator.rs::push_switch_commit` | 断滑窗、退联想、记统计都在调用链前段做了；检索放宽在 `take_input_on_mode_switch_raw` 清组合时复原 |
| DLL 排水 / 顶码延迟提交 | `message_handler.rs::handle_commit_request` | 死代码：DLL 的 `_SendCommitRequest` 从未调用。接通时构造 `InsertText` 走按键来源，并把它调的 `space_committed_text` 纳入守卫 |

## 5. 组件化方向

原则：**至少两三个真实消费者才抽象**；能用 enum + 常量表达的不用 trait 对象；顺序理由是
「逐对」的地方用直排代码，不用注册表。

### 5.1 后处理步骤表（P4）——已落地，见 `postprocess.rs`

```rust
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Pass { Filter, SingleChar, FreqRerank, Shadow } // 声明顺序 = 规范顺序

// 码为 None 表示本 profile 不含对应步骤；profile 含该步而码缺失时 debug 断言、release 记错并跳过
pub(crate) struct PassCtx<'a> {
    owner: Option<&'a str>,
    freq_code: Option<&'a str>,
    shadow_code: Option<&'a str>,
}

fn run_passes(&self, state: &State, list: &mut Vec<Candidate>, profile: &[Pass], ctx: &PassCtx)
    -> PassOutcome; // 含 user_pinned

pub(crate) mod profile {
    pub(crate) const SENTENCE_POOL: &[Pass] = &[Filter, SingleChar, Shadow];
    pub(crate) const TEMP_ENGLISH: &[Pass] = &[FreqRerank, Shadow]; // 不接过滤的理由写在常量旁
    // …其余见 §4.2
}
```

- 每条路径用一个 profile 常量声明自己接哪几步；单测断言每个 profile 都是规范顺序的子序列。
  顺序约束从注释变成会红的测试。
- `PassCtx` 必须显式传码域与归属方案（各模式的 `apply_*_in` 口径不同），不能回落到活跃方案。
- 主链独有、只有一个消费者的步骤（英文定位、整句块、纠错护顶、直接辅助码、出简让全）留在原地。

### 5.2 统一出口收尾（P6）——已落地，见 `session_exit.rs`

```rust
pub(crate) enum ActionOrigin { Key, Pointer, CapsLockHook, Tooltip }
pub(crate) const fn exit_steps(origin: ActionOrigin) -> ExitSteps; // 每来源一行
fn finish_session_action(&self, st: MutexGuard<State>, origin: ActionOrigin, action: KeyAction)
    -> KeyAction; // 平台中立；TSF 协议相关的仍在 handle_key_event_policed
```

用具名函数而不是钩子列表：出口各步的先后理由是逐对的，列表表达不了。第一步按来源逐字复现
现状（已完成），随后逐项定案补齐（§4.3 表，已登记 changelog）。移动端走内层入口，不建来源，
要统一时另议。

### 5.3 不做

| 不做的事 | 理由 |
|---|---|
| 按键链改注册表 | 顺序即契约，注册表把顺序藏进注册次序 |
| 模式改 trait 对象 | 与 `pipeline.rs` 决定一致；改为去掉以模式为键的 `match` 里的 `_ =>`，新增模式时由编译器列出接入点 |
| 上屏文本变换链 | 两个挂点各只有两个变换，所需输入也不同 |
| 用户可配置的降级链 | key-resolver-unification.md §5 已否决；§5.1 / §5.2 都是内部固定顺序，不冲突 |

### 5.4 演进路线

| 步 | 内容 | 验证 | 状态 |
|---|---|---|---|
| 1 | 本文、按键链槽位编号、§8 现状修正 | 无行为变化 | 完成 |
| 2 | 引入 `Pass` / `run_passes`，替换主链、整句池、补全池、直接辅助码 | 全量测试 `--no-fail-fast`；8 组配置逐键候选导出前后 md5 一致 | 完成 |
| 3 | 临拼、临英、mix 自由英文段、特殊、反查改用 profile | 同上 + 临拼/临英/快捷输入/生僻字/反查 6 组导出前后 md5 一致 | 完成 |
| 4 | 抽 `finish_session_action`，逐字复现现状；缺口逐项定案 | 全量测试；绕过守卫；各来源收尾行为测试 | 完成 |
| 5 | 以模式为键的 `match` 去掉 `_ =>`（18 处），守卫 `source_scan::mode_keyed_matches_have_no_wildcard_arm` | 编译通过；替换时按 `unreachable_patterns` 警告删去已覆盖项，保证与原兜底范围相同；全量测试 | 完成 |
| 6 | 清掉 §6.3 的读盘点 | 计数测试（见 §6.2） | 完成 |

## 6. 输入流程零读盘

**规则**：P0–P6 的任何代码（含每次候选推送、注释与提示渲染）不得读配置文件、方案文件或数据
文件，也不得为此列目录、查 `exists()` / `metadata()`。

**理由**：按键线程上的一次读盘加解析在冷缓存、杀毒扫描、网络盘或机械盘上可达几十到几百毫秒，
表现为偶发卡顿，且很难复现。`schema_type_cache` 的注释记录过一次真实案例：`read_schema` 被
`apply_freq_rerank` 每键调用，叠加词频查询造成可感知卡顿。

### 6.1 允许的取值方式

| 要取的东西 | 正确的来源 |
|---|---|
| 用户配置 | `self.rt()`（`ConfigBundle` 原子快照，热重载时整体替换）。需要派生值时在 `ConfigBundle` 构造时算好 |
| 方案定义里的字段 | `EngineManager` 上带缓存的访问器（`schema_engine_type` / `behavior_for` / `key_actions_of` / `aux_code_settings_cached` 等；注意同名的 `aux_code_settings_of` / `aux_code_settings` **不带**缓存），缓存随 `invalidate_schema` / `reload_from_config` 失效。新字段照此加缓存，不要直接调 `read_schema` |
| 数据文件（词库、表、模型） | 构造或重载时加载；必须懒加载的放后台线程（参照 `ensure_chaizi_async`），按键路径只读已加载的结果，没加载好就按「暂缺」处理 |
| 用户数据写入 | 经 wind-userdata 日志追加：句柄常开、按键路径只写入不 fsync，落盘在后台节拍（`journal.rs::sync_if_dirty`） |

用户显式触发的命令（设置页 RPC、cmdbar `config.get` / `toggle`、菜单切双拼布局、主题列表）不属于
输入流程，可以读盘，但不要在持 `state` 锁时做。

### 6.2 怎么守

`wind_engine::manager::schema_reads_on_current_thread()` 统计本线程读方案文件的次数（按线程计，
后台预热线程不计入，并行测试互不干扰）。`wind-coordinator/tests/input_flow_no_disk_reads.rs`
在码表、拼音、混输三类方案下把同一段输入（打码、整句、分段上屏、分隔符、Esc）打两遍，断言第二遍
计数不变。新增按键路径功能时，把它的典型按键序列加进这个测试。

计数只覆盖方案文件。数据文件（词库、表）的读入目前靠 §6.1 的约定与审查守。

### 6.3 已修复的违规（2026-11-13 排查）

`EngineManager::read_schema` 每次调用都要：查文件存在 → 读方案 TOML → 解析 → 列发现目录并读
每个词库文件头 → 读合并 override 文件。排查时以下调用点在按键路径上且没有缓存，现已修复：

| 调用 | 原触发条件 | 修法 |
|---|---|---|
| `mixed_primary_schema` | 混输方案每次按键（调频取码表归属 id、候选推送取 `code_source_schema`、联想取词方案） | 走 `schema_def_cache` |
| `mixed_secondary_schema` | 混输分段上屏后，剩余编码每次重建候选 | 走 `schema_def_cache` |
| `pinyin_separator_mode_of` | 每次判断手动分隔符键 | 走 `schema_def_cache` |
| `aux_code_settings_of` / `aux_code_settings` | 按辅助码触发键进入辅助码模式时 | `enter_aux_code` 改用 `aux_code_settings_cached_of` / `aux_code_settings_cached` |
| `chaizi_spec` + 拆字库路径解析 | 运行期格式串第一次要拆字时（`ensure_chaizi_async`），在调用线程上读方案、查库文件存在 | 方案字段走 `schema_def_cache`，路径解析挪进加载线程 |
| 联想下一词表 | 智能联想档第一次出联想时在按键线程同步读入约 2 MB 文本 | `DataNeeds::assoc_bigram` 驱动后台加载（`ensure_assoc_bigram_async`）；按键路径只取已加载的表，没加载好按暂缺处理 |

`schema_def_cache` 缓存合并 override 后的方案定义，随 `invalidate_schema` / `reload_from_config`
整表清，只供取方案级标量字段；词库清单等随词库目录变化的字段不要从这里取（见字段文档）。
清表后由 `EngineManager::warm_hot_schema_fields` 在非按键线程预填（启动预热、`apply_data_needs`、
切方案），失效后的第一键也不读盘。

缓存的代价：绕过保存入口**手改**用户目录下的方案或 override 文件，要等下一次配置重载才生效——
与既有的 `schema_type_cache` / `aux_settings_cache` 同一口径。设置页、移动端、CLI、方案包导入、
备份还原的写入都经 `invalidate_schema` 或 `reload_from_config`，不受影响。
