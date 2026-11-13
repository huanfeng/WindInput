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
| **P6 出口收尾** | 凡上屏或凡组合结束都要做的事 | `handle_key_event_policed` 后置段；§4.3 的非按键出口 | 与具体候选无关，或依赖本键最终的 `KeyAction` | 只挂出口，不散到 40 多个返回点；会话层判定先于显示层加工 |

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
7. **它在上屏后或组合结束后有副作用吗？** → P6。挂按键出口，并对 §4.3 的非按键出口逐条
   回答「接」或「不接及理由」。
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
| 中英补空格 | 问 6 否 → 问 7 | P6：入口锁存光标前一字、出口追加空格；§4.3 的四处非按键出口各补了一份 |
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

| 路径 | 函数 | 接了哪几步 |
|---|---|---|
| 主链 | `build_candidates` | 全部 |
| 整句池 | 同上，`sentence_pool` | 过滤、单字、shadow |
| 空码补全池 | 同上，`completion_pool` | 过滤、单字、shadow |
| 直接辅助码前缀池 | `handle_direct_aux.rs::decode_direct_prefix` / `merge_direct_hits_into` | 过滤、单字、调频；合并后 shadow |
| 临时拼音 | `handle_temp.rs::refill_temp_pinyin_candidates` | 过滤、调频、shadow |
| 临时英文 | `handle_temp.rs::refill_temp_english_candidates` | 调频、shadow |
| mix 融合 | `handle_mode.rs::refill_mix_candidates` / `mix_free_english_segment` | 调频、shadow（刻意不接过滤，理由见源码注释） |
| 特殊 / 生僻字 / 反查模式 | `handle_special.rs::update_special_candidates` | 调频、shadow |
| 候选反查 | `handle_reverse.rs::build_reverse_candidates` | 过滤 |

翻页扩容（`expand_candidates`）经 `build_candidates_at_cursor` 回到主链整链重建，不是独立路径。
已有两条源码扫描测试兜这张表的一部分（`handle_candidate.rs` 里断言「emoji 三条路都接」与
「shadow 取码同源」的那两条）；§5.1 落地后由 profile 常量取代。

### 4.3 上屏出口（现状）

按键路径的出口是 `handle_key_event_policed` 的后置段，顺序：上屏码重新分派 → 中英补空格 →
换行改写 → 自提交打点与码表造词投喂 → hold 清理 → 智能符号武装失效 → 检索放宽复原 → 配对保活 →
联想占位收口。

不经这个出口的上屏路径：

| 路径 | 位置 | 现在做了什么 |
|---|---|---|
| CapsLock 钩子上屏 | `coordinator.rs::handle_capslock_hook_press` | 上屏码重新分派、补空格 |
| 鼠标点选（主路 / 模式路） | `handle_candidate.rs::select_candidate_at` 一带 | 补空格、解除智能符号武装 |
| 提示框上屏 | `handle_tooltip.rs` | 只补空格 |
| 移动端 | `wind-mobile/src/lib.rs` 直调内层 `handle_key_event` | 全部绕过 |

这几条都缺换行改写、造词投喂、检索放宽复原、配对保活、联想占位收口中的大部分，哪些是有意、
哪些是漏接尚未逐项定案（§5.2）。

## 5. 组件化方向

原则：**至少两三个真实消费者才抽象**；能用 enum + 常量表达的不用 trait 对象；顺序理由是
「逐对」的地方用直排代码，不用注册表。

### 5.1 后处理步骤表（P4）——要做

```rust
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Pass { Filter, SingleChar, FreqRerank, Shadow } // 声明顺序 = 规范顺序

pub(crate) struct PassCtx<'a> { owner: Option<&'a str>, freq_code: &'a str, shadow_code: &'a str }

fn run_passes(&self, state: &State, list: &mut Vec<Candidate>, profile: &[Pass], ctx: &PassCtx)
    -> PassOutcome; // 含 user_pinned

const SIDE_POOL: &[Pass] = &[Pass::Filter, Pass::SingleChar, Pass::Shadow];
const MIX_MEMBER: &[Pass] = &[Pass::FreqRerank, Pass::Shadow]; // 不接过滤的理由写在常量旁
```

- 每条路径用一个 profile 常量声明自己接哪几步；单测断言每个 profile 都是规范顺序的子序列。
  顺序约束从注释变成会红的测试。
- `PassCtx` 必须显式传码域与归属方案（各模式的 `apply_*_in` 口径不同），不能回落到活跃方案。
- 主链独有、只有一个消费者的步骤（英文定位、整句块、纠错护顶、直接辅助码、出简让全）留在原地。

### 5.2 统一出口收尾（P6）——要做

```rust
pub(crate) enum ActionOrigin<'a> { Key(&'a KeyEventData), Pointer, Hook, Tooltip, Mobile }
fn finish_session_action(&self, origin: ActionOrigin, action: KeyAction) -> KeyAction; // 平台中立
fn finish_desktop_key(&self, data: &KeyEventData, action: KeyAction) -> KeyAction;     // TSF 协议
```

用具名函数而不是钩子列表：出口各步的先后理由是逐对的，列表表达不了。第一步只按来源逐字复现
现状；§4.3 的缺口逐项另行提交，每项登记 changelog。

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
| 2 | 引入 `Pass` / `run_passes`，替换主链、整句池、补全池、直接辅助码 | 全量测试 `--no-fail-fast`；`pinyin_eval` 前后对拍 | 未开始 |
| 3 | 临拼、临英、mix、特殊、反查改用 profile | 同上 + 各模式测试 | 未开始 |
| 4 | 抽 `finish_session_action`，逐字复现现状；缺口逐项定案 | smart_symbol / auto_spacing / freq_learn 测试 | 未开始 |
| 5 | 以模式为键的 `match` 去掉 `_ =>` | 编译通过 | 未开始 |
| 6 | 清掉 §6.3 的读盘点 | 计数测试（见 §6.2） | 未开始 |

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

计划在 `EngineManager` 的 `read_schema_in` 与数据文件加载入口加调用计数（仅测试构建），
在 `tests/input_flow` 一类集成测试里断言「预热后连续打一段字，计数不变」。规则先靠本文与审查，
计数断言随 §5.4 第 6 步落地。

### 6.3 现存违规（2026-11-13 排查）

`EngineManager::read_schema` 每次调用都要：查文件存在 → 读方案 TOML → 解析 → 列发现目录并读
每个词库文件头 → 读合并 override 文件。以下调用点在按键路径上且没有缓存：

| 调用 | 触发条件 | 位置 |
|---|---|---|
| `mixed_primary_schema` | 混输方案**每次按键**：`apply_freq_rerank` 取码表归属 id（`write_data_schema_id`）；每次候选推送：`code_source_schema`（其注释写「不读盘」，混输下不成立）；联想取词方案 `assoc_word_schema` | wind-engine `manager.rs` |
| `mixed_secondary_schema` | 混输分段上屏后，剩余编码每次重建候选（`build_candidates` 开头）；注释写「按键路径上用」，但无缓存 | 同上 |
| `pinyin_separator_mode_of` | 每次判断手动分隔符键（`manual_separator_key`：按 `'` / `` ` `` 时、符号缓冲闸门、整句切换键冲突检查） | 同上，经 `handle_candidate.rs::manual_separator_vk_of` |
| `aux_code_settings_of` / `aux_code_settings` | 按辅助码触发键进入辅助码模式时（`handle_aux_code.rs::enter_aux_code`）；逐键筛选已改用 `aux_code_settings_cached` | 同上 |
| 联想下一词表 | 智能联想档**第一次**出联想时在按键线程同步读入并解析约 2 MB 文本 | `handle_assoc.rs::assoc_bigram` |

修法统一：前四项并入 `EngineManager` 现有的按方案缓存（与 `schema_type_cache` 同批失效）；
联想表改为构造 / 重载时加载，或参照 `ensure_chaizi_async` 后台加载。
