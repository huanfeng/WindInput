<!-- Parent: ../../AGENTS.md -->
<!-- Updated: 2026-07-06 -->

# wind-engine

## Purpose
输入引擎层：把用户编码转成候选词。Schema 驱动的引擎工厂——按方案 TOML 的 `engine_type` 构建四类引擎（拼音（含双拼）/ 码表 / 混输 / 英文），由 `EngineManager` 统一懒加载、切换并分发 `convert`。下游消费 wind-config（方案定义）、wind-dict（词库查询）、wind-store（用户/临时词频），上游被 coordinator 调用。
从词库到候选的全链路现状文档见 [docs/architecture/engine-candidate-pipeline.md](../../../docs/architecture/engine-candidate-pipeline.md)。

## Key Files
| File | Description |
|------|-------------|
| `src/lib.rs` | 对外导出：`EngineManager`、`Engine`/`ExtendedEngine`/`ConvertResult`/`EngineType`、四类引擎、`FreqSettings`/`FreqStrategy` |
| `src/engine.rs` | `Engine`/`ExtendedEngine` trait 与 `ConvertResult`（候选 + preedit + 上屏/分段语义字段）；trait 默认方法把顶码、造词出码、扩展词库热插拔做成可选能力 |
| `src/manager.rs` | `EngineManager`：方案注册表 + `build_engine` 工厂。懒加载、`switch_schema`/`cycle_schema`、`reload_from_config`/`invalidate_schema` 热更新、override 层深合并、主码表反查索引、双拼韵母键集、词频设置解析 |
| `src/freq_rerank.rs` | 词频重排（排序独立维度，**绝不改 weight**）：码表/混输 `rerank_codetable_usedfirst`（档位感知永久 used-first）、拼音 `rerank_pinyin_decay`（衰减软置前 + 整句豁免 + 阈值褪色）。由 coordinator 在引擎排序后调用 |
| `src/pinyin/mod.rs` | `PinyinEngine`：精确 → Viterbi 整句 → DAG 子短语 → 前缀补全 → 简拼 → store 造词层，按层级排序（完整 >> 子短语 >> 前缀 >> 模糊）。子模块 `dag`/`viterbi`/`lattice`/`lm`/`scorer`/`fuzzy`/`syllable`/`shuangpin`/`generate`/`parser` |
| `src/codetable/engine.rs` | `CodeTableEngine`：经 `DictManager`(CompositeDict) 精确 + 前缀查询；全码自动上屏、顶码、`clear_on_empty_max` 等上屏策略（`CommitOptions`） |
| `src/mixed/engine.rs` | `MixedEngine`：持码表主 + 拼音次 + 可选英文子引擎，分档加权合并（码表精确 +boost、短语 +1M、英文精确 +500K、前缀 +500K，拼音 ÷100 降档）；**拼音否决统一入口 `pinyin_vetoes_commit`**（否决①粗粒度默认关 / ②词强度默认开，满码/顶码/显示态复评三通路共用）；超码长走 `convert_overflow`（`pinyin_only_overflow` 分流），归属由 `codetable_owns_overflow` 四条判据裁决 |
| `src/english.rs` | `EnglishEngine`：码表引擎薄包装（词库 code 列小写化，大小写不敏感前缀匹配），独立方案或被混输懒加载（`schema.mix.enable_english`） |
| `src/english_merge.rs` | 英文候选混入（**只收精确命中**）：配置按引擎分两份——`schema.codetable.english_merge`（三项，可经方案级 `[engine.codetable.english_merge]` 覆盖）与 `schema.pinyin.english_merge`（两项，无 `block_commit`），由 `manager::english_merge_cfg` 分流折叠成 `Effective`。接线在 `manager.rs` 的三条通路，**不是**混输那套（后者与 `truncation_tier` 三方仲裁耦合，见该模块文档） |

## For AI Agents

### Working In This Directory
- **引擎构建唯一入口是 `manager.rs::build_engine`**：读方案 TOML（用户目录 > 安装目录，再深合并 `schema_overrides/{id}.toml`），按 `engine_type` 分派；mixed 递归构建 primary/secondary 子引擎。新增方案字段须在此解析，并考虑是否需在 `reload_from_config` 热更新（否则改设置要重启才生效）。
- **码表行为分层（仅码表有 override）**：上屏等行为解析顺序为 方案 `schema_overrides/{id}.toml [codetable]`（带 `enabled` 总开关，逐字段 `Some` 覆盖）> 全局 `schema.codetable` > 内置默认；统一经 `CodetableGlobal::resolved()` / `resolve_codetable()`。拼音、混输**无方案 override**，只读全局 `schema.pinyin` / `schema.mix`。混输的码表类行为继承主码表 `schema.codetable`。调频/造词全局唯一按引擎分（`schema.codetable.frequency` / `schema.pinyin.frequency`）。详见 `docs/redesign/schema-config-layering.md`。
- **词频是与 weight 解耦的独立维度**：引擎 `convert` 只产出基础权重候选；`freq_rerank` 是 coordinator 排序后调用的纯函数，**不得在引擎内改 weight 做词频**。两套语义（码表永久 used-first / 拼音衰减褪色）不可混用。
- **拼音 vs 码表的根本差异**：拼音走连续解码（DAG 分词 + Viterbi 打分 + 层级排序，节点分取自词条自身的词典权重），码表只做 `DictManager` 精确 + 前缀查表无评分。匹配层级的**唯一真相**是 `wind_candidate::cmp_match_layers`（`is_abbrev`/`is_prefix`/`is_partial`），引擎层、协调器 `candidate_display_order`、`freq_rerank` 三处统一调用它，勿再各写一份。
- **「层级」与「来源」必须分开**：层级键是布尔的，等价于「惩罚 = ∞」，只该用于结构性的匹配质量差异。召回**来源**（模糊音 `is_fuzzy`、用户词 `meta.is_user_dict`）一律走 weight 上的惩罚/加成，不得塞进 `cmp_match_layers`。`is_fuzzy` 曾是其首要键，真实词库下把模糊候选整体压到 200 名开外（`si` 下「是」第 231 位，而生产候选上限 50~300），模糊音在拼音/混输/临拼三条路径上全部等价于未实现；现改为 `FUZZY_WEIGHT_SCALE` 折扣。同理 `is_prefix` 被静态短语、`is_fuzzy` 被用户词简拼借作「沉底」标记都已拆出独立字段（`is_promoted_completion` / `is_abbrev`）——**要沉底就加自己的字段，别借现成的布尔**。
- **整句候选不吃比例折扣**：Viterbi 整句带 `SENTENCE_WEIGHT_BASE`(3e7) 基准分，与词频量纲（1e2~1e6）差几个数量级，任何 `weight * k` 都压不到同一区间。整句要降位只能走 `is_sentence_demoted`（降到精确整词之下）。
- **懒加载 + single-flight 构建锁**：`ensure_loaded` 抢方案专属 build_lock 后复查，避免后台预热与首次切换重复熔大词库；不同方案可并行构建。引擎缓存仅在 `invalidate_schema`/`reload_from_config` 清除（无 LRU 驱逐，与 Go 版不同）。
- **`convert` 永不 panic**：引擎错误降级为 `ConvertResult::default()`（空候选），勿在热路径用会 panic 的 `unwrap`。锁中毒统一 `unwrap_or_else(|e| e.into_inner())`。
- **扩展词库热插拔**：`set_dict_enabled` 直接翻 `codetable-extra-<id>` 系统层的 enabled 标志，无需重建引擎；启用集变化会失效反查索引（编码提示依赖启用词库合并）。
- **混输拼音否决默认值三处同源**：`auto_commit_block_on_pinyin` / `block_commit_on_pinyin_word` / `pinyin_only_overflow` **均默认开**——`MixConfig::default()`（mixed/engine.rs）、`MixGlobal::default()` + serde default（wind-config/config.rs）、`data/config.toml [schema.mix]` 三处必须一致，改默认须同步全部三处。**出厂默认以 L1⊕L2 为准**（L2 覆盖 L1，即 data/config.toml 的值）。前两者曾漂移过（`MixConfig` false vs 另两处 true / `pinyin_only_overflow` L1 false vs L2 true），后果分两层：引擎单测跑在现实中不存在的配置下；且 `Config::preset_for_pruning` 拿 L1⊕L2 判「用户值是否等于默认」，L1/L2 不一致时该判据会开始吃用户配置。
- **★ 混输上屏有三条通路，任何否决开关必须三处都接**：① `convert` 的满码上屏（`should_commit`）、② `recheck_auto_commit` 的显示态复评、③ `handle_top_code` 的顶码。**协调器让顶码先于候选刷新执行**（`coordinator.rs` 字母键臂），所以第三条漏读一个开关，该开关对超码长输入就等于完全失效——而它在满码路径上工作正常，日志与设置页均无痕迹。已因此栽过两次：`pinyin_only_overflow` 只被 `convert` 读（youyoud→顶出「变凉」，补否决⓪）、`auto_commit_block_on_english` 只有前两个使用点（github 打到第 5 键顶出「不算」，补否决③）。`coordinator.rs` 那道「显示首选是拼音/英文就放弃顶码」的保护指望不上：码表精确 +1e7 vs 英文精确 +500K，非码表候选永远排不到第一。
- **★ 超码长归属 `codetable_owns_overflow` 有四条判据，前三条问「谁解释得了整串」（前 N 码是精确全码 / 拼音主张不了 / 英文无精确整串词条），第四条问「这串还算不算中文」（拼音至少交得出候选）**。第 2 与第 4 条方向相反，两头夹出「还在中文语境里、但拼音接管不了整串」的窄带（`yijga` 出得来「以」却只解释 2/5，落在带内；`github` 什么都出不来，落在带外 ⇒ 候选留空让用户上屏原码）。判据 3、4 是 `github` 回归的修复：前者管开着英文词库的场景（码表精确 +1e7 压掉英文 +500K），后者管关着的场景。**该函数被顶码 ⓪ 与候选装配共用，加判据时两边都要重估**：判据 4 对顶码无影响（⓪ 需 `has_pinyin`，为 false 时整条不成立），判据 3 与顶码侧的 ③ 是不同维度（③ 是上屏否决开关、出厂 false；判据 3 管候选归属、不读任何开关）。
- **★ 回捞的前缀候选是「码表候选带 `consumed_length` 的唯一出口」**：它只解释得了前 N 码，不标的话协调器 `commit_selected` 的 `partial = consumed > 0 && consumed < total` 恒为 false ⇒ 按「消费整串」上屏，选中即把尾码吃掉（`yijga` 选「就是」→ `a` 消失）。协调器侧两处原本依赖「码表恒 0 ⇒ 永不部分匹配」的判据已随之对齐：`build_candidates` 的分段续转改看**最后一段来源**、`learn_phrase_on_commit` 对全段码表显式跳过。改这条务必回头核那两处。
- **★ 上屏三通路的「英文守护」在 `EngineManager` 上有第二份实现，改一处要想到另一处**：英文混入（`schema.codetable.english_merge` / `schema.pinyin.english_merge`）把同一套否决接在 `manager.rs` 的 `convert` / `recheck_auto_commit` / `handle_top_code` 三个方法上——那里是三条通路的**共同收口点**。⚠️ 否决只对码表侧有意义（拼音无满码上屏/顶码），故 `Effective::block_commit` 在拼音侧恒 `false`。混输的那份（`schema.mix.*`，接在 `MixedEngine` 内部）与它**互不接管**、按引擎类型分流（`english_merge_ctx` 显式排除 Mixed/English），故两份不会叠加生效。判据同源要求不变：三处都必须叠「英文确有候选」。
- **顶码否决必须叠「对方确有候选」**：⓪ 叠 `has_pinyin`、③ 叠 `!english_candidates(input,1).is_empty()`。只看开关就禁顶码会把「顶码抢了别人的活」修成「谁的活都没人干」——纯五笔溢出串（`aaaab`）在 `pinyin_only_overflow=true` 下 overflow 侧只查拼音、同样交不出候选，用户卡在既不上屏又无候选的长串上。③ 天然满足（判据与 `convert_overflow` 调同一个 `english_candidates`、同一个 input）。③ 还须放在 `if let Some(sec)` 块**外**：英文守护与拼音子引擎无关，纯码表+英文混输也要生效。

### Testing Requirements
- 纯逻辑，host 可跑：`cargo test -p wind-engine`（单元测试 + `tests/engine_manager.rs` 集成测试；部分用例读 `data/schemas/` 真实数据文件）。
- 传递依赖的 wind-dict 仅在 `cfg(windows)` 下引入 `windows` crate；本仓开发机即 Windows，host 直接构建/测试即可，无需交叉编译或上设备。

## Dependencies

### Internal
- `wind-candidate` — `Candidate`/`CandidateSource`/`better` 排序
- `wind-dict` — `CachedDict`、`DictManager`/CompositeDict、`SystemDictLayer`/`StoreUserLayer`/`StoreTempLayer`、wdat/wcmt mmap
- `wind-store` — redb 用户/临时词库与词频记录（`FreqRecord`/`FreqProfile`）
- `wind-config` — `Config`、`schema::Schema`/`DictSpec`、`CodeCommitConfig`、`PinyinGlobalConfig`

### External
- `tracing`（日志）、`anyhow`/`thiserror`（错误）、`serde`/`serde_yaml`/`toml`（方案与 override 解析）

## 全局约束
- 引用根 `AGENTS.md`：INFO 级日志只记方案 id / 条目数 / key 数，**不得含候选文本、用户输入或词库内容**；改完跑 `cargo fmt`。

<!-- MANUAL: 此行以下为人工补充区，重新生成时保留 -->
