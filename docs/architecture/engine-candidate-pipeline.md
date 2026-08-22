# 输入引擎架构：从词库到候选（现状整理）

> **现状文档**（非设计差分）。基于 2026-07-06 对 `wind_input/crates/` Rust 实现的实际代码核查整理，
> 覆盖码表 / 拼音（全拼）/ 双拼 / 混输 / 英文五类引擎从词库加载到候选呈现的完整链路，
> 并对比各模式流程差异；§9 补辅助码字形二次筛选（协调器 overlay 模式）。
> 行号为核查时快照，随代码演进可能漂移，以函数/结构名为准。
>
> 历史设计差分见 [redesign/engine.md](../redesign/engine.md)（2026-06-15，其中记录的 Rust 现状已过时）。

---

## 1. 总览：统一入口与公共契约

### 1.1 分层结构

```
按键 (wind-coordinator)
  └─ build_candidates()                    handle_candidate.rs —— 候选后处理管线（§8）
       └─ EngineManager::convert()          wind-engine/manager.rs —— 按活跃方案分发（§1.3）
            └─ dyn Engine::convert()        五类引擎实现之一（§3–§7）
                 └─ DictManager / CompositeDict   wind-dict —— 多层词库合并查询（§2）
```

### 1.2 公共契约

**`Engine` trait**（`wind-engine/src/engine.rs`）：核心方法 `convert(input, max_candidates) -> ConvertResult`，
另有 `reset()` / `engine_type()` / `max_code_length()` / `handle_top_code()` / `recheck_auto_commit()` /
`set_dict_enabled()`，以及拼音探测类方法 `is_whole_syllable_pinyin()` / `completed_syllable_count()` 等
（混输的拼音否决依赖后者，§7.3）。

**`EngineType`**：`Pinyin`（全拼/双拼共用）/ `CodeTable` / `Mixed` / `English`。

**`ConvertResult`**（engine.rs:18-42）：

| 字段 | 含义 |
|---|---|
| `candidates` | 引擎排序后的候选列表 |
| `preedit_display` | 组合区显示串（拼音含 `'` 分隔；码表为原始码） |
| `preedit_pinyin` | 拼音音节拆分形态（混输「高亮跟随」按高亮候选类型选原始码/拆分串） |
| `completed_syllables` / `partial_syllable` / `has_partial` | 拼音音节完成度（UI 用） |
| `should_commit` / `commit_text` | 全码自动上屏意向（协调器复核后才放行） |
| `should_clear` | 满码空码清空缓冲 |
| `is_empty` | 无候选 |

**`Candidate`**（`wind-candidate/src/candidate.rs:28-126`）关键字段：

- `text` / `code` / `comment`（编码提示或「拼」来源标记）
- `weight`（引擎权重，排序主键）/ `natural_order`（同权重自然序，含词库层偏移）
- `source: CandidateSource`（`CodeTable` / `Pinyin` / `English` / `Phrase` / `None`）——贯穿词频记账、
  智能过滤分组、上屏守护的核心标记
- 分类标志：`is_phrase` / `is_command` / `is_group`（短语系）、`is_fuzzy`（模糊音）、
  `is_prefix`（前缀补全，code 比输入长）、`is_partial`（子短语，code 比输入短）、`is_common`（常用字表，过滤用）
- `consumed_length`：候选上屏时消费的输入字节数；`0` 表示整串。拼音分段上屏的基石（§4.5）
- `meta`：`lexicon_name` / `is_user_dict` / `is_temp_dict` / `raw_weight` / `freq_boost`

候选比较函数 `wind_candidate::better()`（candidate.rs:131-139）：
`weight desc → natural_order asc → code asc → consumed_length desc → text asc`。

### 1.3 EngineManager：懒加载与方案分发

`wind-engine/src/manager.rs`（~2570 行）：

- 持 `HashMap<方案ID → Arc<dyn Engine>>`，**懒加载**：`active_engine()`（:507）按需触发
  `ensure_loaded()`（:453），single-flight 构建锁保证并发下只构建一次。
- 统一入口 `convert()`（:991）分发当前活跃引擎；`convert_with(schema_id, ...)`（:1177）指定方案转换
  （混输分段上屏、临时拼音用）。
- `switch_schema()` / `cycle_schema()` / `reload_from_config()`（配置热重载清缓存重建）。
- 引擎构建 `build_engine()`（:1272-1530）按方案 TOML 的 `[engine] type` 分流：

```toml
[schema]
id = "wubi86_pinyin"
[engine]
type = "mixed"              # codetable | pinyin | mixed | english
[engine.pinyin]
scheme = "shuangpin"        # 双拼方案的判定方式（quanpin/shuangpin）
[engine.pinyin.shuangpin]
layout = "xiaohe"           # 双拼布局（data/schemas/shuangpin/*.toml）
[engine.mixed]
primary_schema = "wubi86"   # 混输主（码表）
secondary_schema = "pinyin" # 混输次（拼音）
[[dictionaries]]
path = "dicts/xxx.dict.yaml"
default = true              # 主库标志；非 default 为扩展库
```

- 词频写分流 `write_data_schema_id()`（:734-745）：混输方案下按候选 `source` 路由——
  码表候选记入主码表方案 id，拼音候选统一折叠到 `"pinyin"`，其余跳过记频。
- 拼音候选编码提示 `codetable_reverse_hint()`（:332-353）：按主码表懒建反查索引，
  拼音候选 comment 填实际码表编码（保证与码表真实码一致，而非按字生成）。

---

## 2. 词库层（wind-dict）

### 2.1 三种存储格式

| 格式 | 文件 | 说明 |
|---|---|---|
| YAML 源 | `.dict.yaml` | RIME 风格 TSV：五笔 `code\ttext\tweight`，拼音 `text\tcode\tweight`。列序按**文件级**判定——头部 `columns:` 声明优先，无声明则整文件投票探测、默认 `text` 在前（`codetable.rs:resolve_columns`）。详见 [rime-dict-loading.md](./rime-dict-loading.md) |
| 二进制 | `.wdb` | Header + KeyIndex + DataSection + StringPool；V3 条目含 order 字段（`binformat.rs`） |
| 双数组 Trie | `.wdat` | Header + Base/Check 数组 + LeafTable + EntryRecords + StringPool + 可选简拼段（AbbrevSection）+ CharMap（`datformat.rs`） |

`.wdat` 支持**零拷贝 mmap 读取**（`WdatReader`）：精确查询 walk 编码验终止符；前缀查询 walk 前缀后
DFS 子树重建完整编码、按权重降序截断；`for_each_entry` 流式遍历（反查索引构建用）。
DAT 从已排序编码列表 BFS 直接构建，峰值内存仅 base/check 两数组。

### 2.2 缓存策略（CachedDict）

`cached.rs`：`enum CachedDict { Mmap(WdatReader), Memory(CodetableDict) }`。
加载时若 `.wdat` 缓存存在且**内容指纹**（sidecar，非 mtime）匹配源文件 → 直接 mmap；
否则加载 yaml → 写 `.wdat` → mmap 重开。缓存根 `%LOCALAPPDATA%\WindInput\cache\{方案}/`。
拼音另有合并缓存：`merged.wdb`（主库+import_tables）。

### 2.3 多层合并（CompositeDict）

`DictManager`（manager.rs:15-52）持一个方案的 `CompositeDict`（composite.rs），层类型优先级
（layer.rs）：**Logic(0) > User(1) > Temp(2) > Cell(3) > System(4)**。

- 系统层 `SystemDictLayer` 包装 CachedDict 并打 `source` 标记；用户造词 `StoreUserLayer`（redb 持久化）、
  临时学习词 `StoreTempLayer`（会话级）来自 wind-store。
- 合并查询：遍历启用层收集候选 → **按 text 去重**（weight 继承最高值；前缀查询同 text 多码取最短码）→
  每层 natural_order 叠加 `layer_idx × PER_LAYER_NO_OFFSET(10M)` 保证层序 → `better()` 排序 → 截断。
- 层可热插拔：`set_layer_enabled()`（码表扩展库 `codetable-extra-*` 开关走此通道）。

---

## 3. 码表引擎（CodeTableEngine）

文件：`wind-engine/src/codetable/engine.rs`（~440 行）。

### 3.1 查询流程（`convert()`，:98-174）

```
输入码
 ├─ ① 精确匹配   dm.search(input)          → source=CodeTable
 ├─ ② 前缀匹配   dm.search_prefix(input)    仅 !single_code_input 时；按 text 与①去重
 └─ ③ 空码补全   search_prefix(input, 8) 取首个 code≠input 的候选
                  仅 single_code_complete 且①②为空且未满码时
 → better() 排序 → truncate（截断保护精确匹配，见下）
 → show_code_hint 时前缀候选 comment 标注剩余编码
 → 自动上屏判定 / 满码空码清空（has_longer_code 单次求值复用）
```

**截断保护精确匹配**：短输入（如单字母）前缀候选可达数百，纯按权重 `truncate` 会把低权重的精确
全码（五笔一/二级简码等 `code==input`）挤出配额丢失。超额时改为「精确优先」稳定分区截断——精确
候选必留、其余按 `better` 序填满剩余配额——再恢复 `better` 显示序。**不持久化 `is_prefix`**：跨来源
混输的截断档位与纯码表显示序均不受影响。

### 3.2 上屏策略（CommitOptions，:14-31）

| 选项 | 行为 |
|---|---|
| `auto_commit_at_full` + `auto_commit_min_len` | 全码自动上屏：码长 ≥ min_len 且**恰一个精确匹配**且**无更长后继**（`decide_auto_commit()` :70；后继判定 `has_longer_code()` :54 用 `search_prefix(input, 64)` 查是否存在更长码） |
| `clear_on_empty_max` | 满码空码清空：无候选且码长 ≥ max_code_length 且无更长后继 → `should_clear` |
| `top_code_commit` | 顶码：见 `handle_top_code()`（:206）——输入**超过** max_code_length 且整串无精确匹配、无更长后继 → 取前 N 码 convert 首选上屏，余码返回续打 |
| `single_code_input` | 精确模式：禁前缀匹配 |
| `single_code_complete` | 精确模式下的空码补全 |
| `show_code_hint` | 前缀候选标注剩余编码 |

配置来源：全局 `schema.codetable.*` + 方案 `[engine.codetable]` 行为字段逐字段折叠（`Some` 覆盖 /
`None` 回落全局）。行为与引擎固定参数**同段同结构**收在 `CodeTableSpec`（`wind-config/src/schema.rs`）：
固定参数 `max_code_length` / `base_sort`（weight/natural）/ `input_chars`，行为参数为 tri-state `Option`。
方案作者可在 `.schema.toml` 内联行为基线；`schema_overrides/{id}.toml` 用**相同的 `[engine.codetable]` 段**
（设置页写入）经 `read_schema` 深合并覆盖之。已无独立 `CodetableOverride` 平行路径。

### 3.3 显示态复评（`recheck_auto_commit`）

引擎按**未过滤**候选判唯一（生僻同码字会导致不唯一而否决）；协调器智能过滤掉生僻字后若显示列表只剩
唯一精确全码 → 据显示候选复评放行（§8 第⑧步）。上屏判定与用户所见保持一致。

---

## 4. 拼音引擎（PinyinEngine，全拼）

文件：`wind-engine/src/pinyin/`。词库：`rime_pinyin` 主库合并 import_tables 缓存为 `merged.wdb` mmap；
用户/临时造词层经 `with_store_layers()` 注入。

> **没有独立的语言模型文件**。词图节点分直接取自词条自身的词典权重（`lattice.rs::score_node_inner`），
> 与 librime 一致。早前那份 `unigram.txt` 是 `cn_dicts` 的一份副本（同源、跨库取 max），
> 双源冗余还引入了多音字污染（「说」按 shui 读音也拿到 shuo 的频次，虚高 6.7 万倍），
> 已连同 `lm.rs` / `UnigramLookup` / `boost_user_freq` 一并移除。

### 4.1 音节切分

- `SyllableTrie`（syllable.rs）：~417 个标准音节的字节级 Trie，`match_at()` 返回某位置全部可能音节。
- `Dag::maximum_match()`（dag.rs）：DP 求**覆盖最多字符**的音节切分（非贪心，如
  `henihejiele → he+ni+he+jie+le`）。
- 分隔符 `'`：硬边界。`segment_with_separators()` 按 `'` 分段各段独立切分；
  `map_consumed_over_separators()` 把 consumed_length 补偿回原始输入空间（mod.rs:325-343）。
- 模糊音（fuzzy.rs）：`FuzzyConfig` 11 个开关（zh_z/ch_c/sh_s/n_l/f_h/r_l + an_ang/en_eng/in_ing/
  ian_iang/uan_uang），`lookup_with_fuzzy()`（mod.rs:221）对各音节变体做笛卡尔积扩展查询
  （组合数 > 64 跳过），命中标 `is_fuzzy`。

### 4.2 候选生成各步（`convert()`，`pinyin/mod.rs`）

> 下表按产出顺序列主干步骤（原标题「六步」已不准——②b/②c 是后加的独立解码路径）。
> 步骤号沿用代码注释里的编号，便于对照；排序与层级的**权威描述**在
> `candidate-sorting-rules.md`，本节只讲「候选从哪来」。

| 步骤 | 内容 | 标志 |
|---|---|---|
| ① 精确查找 | `lookup_with_fuzzy(completed)`——以**完成音节前缀**（去尾部残码）为查询码与存储 code | — |
| ② Viterbi 整句 | `use_smart_compose` 且 ≥2 音节：LatticeBuilder 建词图（`max_word_len=10`，模糊变体 -0.5 惩罚）→ ViterbiDecoder DP 最优路径；权重 = `SENTENCE_WEIGHT_BASE(30M) + clamp(log_prob×1000)`。**只在 `completed`（去尾部残码）上建图** | `is_sentence`，insert(0) |
| ②b 混合整句 | 简拼段与全拼段同图解码（`bzdhaobuhao`→不知道好不好）；在**整串**上建图 + `add_abbrev_nodes` | `is_sentence` |
| ②c **残码整句** | 尾部残码作为**待定音节**入图（`add_partial_final_nodes`），Viterbi 选最优单字：`buzhidaok`→「不知道**看**」。在**含残码的整串**上重建图——step ② 的 `nodes` 只到 `completed.len()+1`，残码末端没有槽位。对齐 librime `enable_completion` / fcitx5 不完整拼音。门槛：≥2 完整音节、非双拼、非分隔符、**非混输**（`enable_partial_final`） | `is_sentence` + `is_sentence_unanchored` |
| ③ DAG 子短语 | 前 6 音节的各前缀子段查词（分段上屏候选） | `is_partial` |
| ④ 前缀补全 | `search_prefix(query, 30)` | `is_prefix` |
| ⑤ 简拼 | `AbbrevMatcher` 判定（每字母为音节首字母且非完整音节序列）→ `search_abbrev(query, 10)` | natural_order=999999 沉底 |
| ⑥ 用户/临时造词层 | store_layers 整串精确 + 子码 + 前缀，按 text 与系统词典去重 | — |

节点打分（lattice.rs `score_node()`）：以 `ln(weight / DICT_TOTAL)` 为基础（`weight` 即词条自身的
词典权重，`w ≤ 0` 走 `0.5/T` 兜底，对齐 librime 的 `DBL_EPSILON` 思路），叠加单字实词惩罚(-3.0)/虚词加成(+2.0)/
多字词典词加成(+3.0×√字数×freq_factor)/OOV 字符均值(-2.0) 等调整。

> ⚠️ **残码待定音节（②c）走 `score_node_partial_final()`，不给单字虚词优待**。虚词优待合计
> **8.0** 的量级差（虚词 +2.0、实词 −3.0、再豁免 `WORD_PENALTY` 3.0），足以碾压任何词频
> 差距：实测补出「中华**让**」而非「中华人」、「你好**们**」而非「你好吗」（让/们在虚词表，
> 人/吗不在）。该优待的前提是「虚词随内容词出现是语法黏着」，说的是**整句内部已成形的搭配**；
> 残码位是「用户打到一半的那个音节」，前提不成立。**同一条加成在两个位置前提不同 ⇒ 按位置
> 区分，不按词性区分。**

> **尾部残码前缀补全上浮**：输入尾带未完成音节时（`meiy` 的 `y`），step ④ 的补全候选
> （`meiyou→没有`）须浮到数百条 step ① 精确子串（没/每/美/…）之上，否则用户翻 15+ 页才见。
>
> ⚠️ **实现已变**：早期做法是**给补全硬标 `is_prefix=false`**，使该字段名不符实（一条码更长的
> 候选却说自己不是补全）。现拆为两个正交字段——`is_prefix` 恒表**结构事实**（码严格长于输入），
> 排序决策由 `is_promoted_completion` 承接，`cmp_match_layers` 取 `eff_prefix = is_prefix &&
> !is_promoted_completion`。上浮不再无条件，三道约束：
> - 距离 ≤ `COMPLETION_UNCONDITIONAL_FLOAT_SYLLABLES`(1)：只补完手头这个音节的才无条件上浮；
> - 距离 ≥2 须过 `COMPLETION_FAR_WEIGHT_FLOOR`(100)；
> - **`weight ≤ 0` 一律降级**（词库对存疑条目的标记，`zhonghuar` 的「种花人」w=0 曾靠距离 1
>   的无条件上浮排到第 2、压过 w=18 的「中华人民」）。
>
> 层内还有 `COMPLETION_WEIGHT_DISCOUNT`(0.5^未输入音节数) 的连续折扣——只有层级而无折扣时，
> 层内只比裸词频，`nih` 下「你会发现」(extra=3) 会压过「你好」(extra=1)。
>
> Viterbi 更新既有条目时同时清除 `is_partial`（整句是完整解读而非子短语），否则 30M 置顶但仍挂
> `is_partial=true` 会被残码补全 `is_partial=false` 反超。

### 4.3 分隔符边界过滤

候选 code 恰落在音节边界时，候选字数必须与所跨音节数一致（mod.rs:624-634）：
`xi'an` 强制 [xi,an] 后，单字「先」(xian, 1 字跨 2 音节) 被剔除；前缀补全不受影响。

### 4.4 排序层级（mod.rs:636-651）

`cmp_match_layers`（`is_abbrev asc → eff_prefix asc → is_partial asc`）`→ weight desc →
natural_order asc`，再截断。权威描述见 `candidate-sorting-rules.md`。

⚠️ 两处与旧版本不同：
- **`is_fuzzy` 已不是层级键**。它曾是首要键（等价惩罚 ∞），真实词典下打 `si` 时「是」被压到
  第 231 位、`zong` 时「中」在第 158 位，而生产候选上限仅 50~300 ⇒ 模糊音在全部三条路径上
  等价于未实现。改由 weight 折扣 `FUZZY_WEIGHT_SCALE`(0.01) 表达。
- **引擎侧刻意不按 `consumed_length` 排序**（协调器才按它排，且是首要键）。这里 `truncate`
  紧随排序，用消费长度当键会让消费更少的候选（`xi'an` 的「西」）被**整批丢弃**而非仅仅排后
  ——librime 的 `Translation` 惰性流式从不全局截断，我们是一次性产生 N 条 + 截断，架构不同。

**裸声母单字优先**：打单个声母（`m`/`n`/`h`/`zh` 等，`syllables` 为空、无完整音节）时候选全为前缀
补全词，纯按词频排会让高频多字词（没有/目前）压过单字（吗/么），不合主流输入法直觉。故裸声母时
给**单字候选**加 `BARE_INITIAL_SINGLE_CHAR_BOOST`(1e7)——高于常规词频（单字基础权重上限 ~2e6）。
（历史注记：此值原本还须刻意低于 `PINYIN_SENTENCE_FLOOR`(2e7) 以免被 `freq_rerank` 误当整句锚定。
**该阈值已废弃**——整句锚定改按 `Candidate::is_sentence` 标记判定，此处不必再避让任何数值线。）**经 weight 表达**（非引擎排序），才能穿过
协调器 `build_candidates` 按 `(is_fuzzy, is_prefix, weight)` 的重排。仅裸声母生效——完整音节输入的
单字已靠 `is_prefix` 精确层级就位（`nihao` 仍 `你好` 优先）。

### 4.5 consumed_length（分段上屏）

code 是 query 前缀 → 只消费前缀长度，剩余拼音继续转换；否则消费整串。
双拼激活时经 `sp_result.map_consumed_length()` 回算双拼键数；含 `'` 时经分隔符补偿（mod.rs:653-672）。

### 4.6 造词反推读音

`generate_word_pinyin()`（generate.rs）三级策略：整词读音笛卡尔积回查命中 → 子词 DP 切分继承读音
（解决长词多音字）→ 逐字代表读音兜底。单字读音索引 `CharPinyinIndex` 从词典自身派生，懒构建。

---

## 5. 双拼（Shuangpin）

文件：`pinyin/shuangpin.rs`（~930 行）。**不是独立引擎**——双拼是拼音引擎的前置转换层：
方案判定 `schema.engine.pinyin.scheme == "shuangpin"`（manager.rs:251-273），构建时
`PinyinEngine::with_shuangpin(converter)` 注入，`convert()` 入口先把双拼键串转全拼，后续管线与全拼完全一致。

- **布局是数据不是代码**：`data/schemas/shuangpin/<id>.toml`（内置 xiaohe / ziranma / mspy / sogou /
  ziguang / abc / shoudao / jiajia），三表：`[initials]` 键→声母、`[finals]` 键→韵母列表、
  `[zero_initials]` 键→零声母音节表（首道用 `[zero_pairs]` 显式键对，键位不规则）。
- 键对转换 `convert_pair()` 三层：零声母（韵母交集/字面/matchesFinal）→ 常规声母+韵母
  （含 z↔zh/c↔ch/s↔sh 对偶兜底 `fuzzy_initial_partners()`）→ 重复键单音节（aa→a，
  **受 `[zero_initials]` 约束**，见下）。
- **零声母有两个流派，不可互抄**：微软 / 搜狗 / 智能ABC / 紫光用 `O` 引导（`oj`=an、`oh`=ang），
  自然码 / 小鹤用首字母引导（单韵母重复 `aa`/`ee`/`oo`、双字母韵母打字面 `ai`/`an`、
  三字母韵母 `ah`/`eg`）。微软/搜狗曾整段抄自首字母引导的模板，官方 10 个击键全部打不出而
  覆盖率门禁照绿 —— 门禁只问「打不打得出」，问不到「**官方**击键打不打得出」，
  该层由 `tests/shuangpin_coverage.rs::official_zero_initial_strokes_work` 正向击键表把守。
- 奇数尾键作 partial 声母前缀（has_partial）。
- **位置映射**：每个转出的全拼字节记录双拼原始区间（`ConvertedSyllable{sp_start..sp_end, fp_start..fp_end}`），
  `map_consumed_length()` 使分段上屏语义在双拼键空间成立。
- preedit：双拼激活时组合区显示**原始按键**（按音节边界 `'` 分隔，`build_raw_preedit()`）；
  且剥除手动分隔符（`'` 仅全拼方案支持）。
- 选词热键避让：manager 缓存双拼韵母键集 `shuangpin_final_key()`（manager.rs:278-301）。

---

## 6. 英文引擎（EnglishEngine)

文件：`wind-engine/src/english.rs`（64 行）。**码表引擎的薄包装**：词库用码表格式
（`type = "english"` 方案），构建时 code 列**小写化**实现大小写不敏感前缀匹配（manager.rs:1375-1400）；
查询走精确 + 前缀，候选标 `source = English`。独立方案可直接使用，更常见的是被混输懒加载
（`schema.mix.enable_english`）。

---

## 7. 混输引擎（MixedEngine）—— 冲突处理与拼音否决

文件：`wind-engine/src/mixed/engine.rs`（~1030 行）。部件：`primary`（码表）+ `secondary`（拼音，可空）+
`english`（可空），策略参数经 `MixConfig` 注入（manager.rs:1286-1369 从 `schema.mix.*` 构造）。

### 7.1 输入路由（两条路径）

```
convert(input):
  input_len > max_code_len ──→ convert_overflow()（超长分支，§7.2）
  否则 ──→ 常规合并路径：
     码表全量查询（保存 should_commit 意向）
     + 拼音查询（仅 input_len ≥ min_pinyin_length，默认 2；短输入自然退化为纯码表）
     + 英文查询（enable_english 且 input_len ≥ min_english_length）
     → 加权 → 合并排序去重 → 上屏重评
```

### 7.2 冲突处理 ①：权重档位（双向夹击）

不同来源候选靠 `MixedEngine::truncation_tier` 的**截断优先级档**隔离，`weight` 一律保持真实词频：

| 档 | 对象 |
|---|---|
| 0 | 码表精确（`code == 判据串`） |
| 1 | 短语（**本引擎恒不可达**：短语由协调器在引擎之后合并） |
| 2 | 码表前缀补全、英文整词 |
| 3 | 拼音全部、英文前缀 |

⚠️ 本档位只决定**谁活过截断**，不决定显示序（后者由协调器 `candidate_display_order` 无条件重排）。

合并 `merge_sort_dedup()`：码表在前、拼音在后、英文混入 → **按档稳定排序**（同档保持传入次序
＝子引擎原序，档内不得再排）→ **按 text 去重（保留首个）** → 截断（拼音有 `max/5` 保底配额，
英文没有）。

> **历史**：档位从前编码在 weight 的数值大小里（码表精确 +1e7、短语 +1M、码表前缀补全与英文
> 整词各 +500K、拼音 ÷100），已整体拆除，见 `docs/design/mixed-source-tier-quota.md`。

### 7.3 冲突处理 ②：拼音否决（veto）—— 统一入口

**统一判据 `pinyin_vetoes_commit(input, has_pinyin)`**（engine.rs:170-172），满码/顶码/显示态复评三条
上屏通路**共用同一套**（提交 847ca08 统一），杜绝「满码不否决、顶码却否决」的不一致：

```rust
(auto_commit_block_on_pinyin && has_pinyin) || is_ambiguous_pinyin_word(input)
```

**① 粗粒度守护 `auto_commit_block_on_pinyin`**：只要整串存在拼音候选就否决码表上屏。
**默认开**（三处同源：`MixConfig::default()` / `MixGlobal::default()` / `data/config.toml`）。
它同时是**满码空码清空**的总闸（见下方「空码清空」），关闭 = 拼音一律不干预码表处置。

⚠️ **① 在顶码通路上有一个例外口**：超码长且 `codetable_owns_overflow(input)` 成立时，① 不再
以「有拼音候选」为由否决（与 ⓪ 共用同一个归属结论，见 §7.5）。语义依据是 ① 的含义为**让路给
拼音**，而让路的前提是拼音接得住这一串 —— 归属判据已判定它接不住。没有这个例外口时，同一次
按键里候选侧已把码表词回捞到首位、顶码侧却仍被 ① 拦下，两处对同一个归属问题给出相反处置。
真机实例：`cety`（唯一全码「通往」）+ 第 5 键，`ce` 是完整音节而 `ty` 连音节前缀都不是，拼音
只解释 2/5 键。**② 不享受该豁免**（词强度与归属正交，两者判据也基本互斥）。

**② 拼音词拦截 `is_ambiguous_pinyin_word()`**（engine.rs:134-163，`block_commit_on_pinyin_word`
控制，默认**开**），命中任一即判「用户意图是拼音」：

- **(b) 单音节前缀（中途态）**：前 max_code_len 码的前缀**恰是 1 个完整拼音音节**
  （`is_whole_syllable_pinyin(prefix) && completed_syllable_count(prefix)==1`）→
  用户多半正在打拼音词中途（wangb→wangba→网吧），拦。
  这是区分 `wang`（1 音节，拦）与 `aipu`（ai+pu 2 音节，多为恰好像拼音的五笔码，放行「落实」）的关键。
- **(a) 整串强拼音词**：整串是完整音节序列，且拼音引擎首选是「≥2 汉字、消费整串
  （consumed_length==0 或 ≥ 整串）、weight ≥ `pinyin_word_min_weight`」的真实词——
  借拼音引擎自身排序识别强词（wangba→网吧 拦）。

**三条通路的接线**：

| 通路 | 位置 | 否决方式 |
|---|---|---|
| 满码自动上屏 | `convert()` engine.rs:441-450 | 取码表意向后：`!has_english && !pinyin_vetoes_commit(...)` 且上屏目标在合并结果中**存活**才放行；否决短路求值（仅码表确有意向时才跑拼音转换） |
| 顶码上屏 | `handle_top_code()` engine.rs:961+ | 超码长时先查整串拼音得 has_pinyin，`pinyin_vetoes_commit(input, has_pinyin && !codetable_owns)` 命中 → 返回 None（放弃顶码继续组合）；`codetable_owns` 即 ⓪ 的例外口 `codetable_owns_overflow`，**⓪① 共用**；`top_code_override_pinyin=true` 时**无视否决**强制倒向码表 |
| 显示态复评 | `recheck_auto_commit()` engine.rs:485-502 | 先按显示候选来源算 has_pinyin/has_english 走同一套否决（复评**不绕过**否决），再仅取 `source==CodeTable` 的候选委托主码表判唯一 |

配套：**英文守护** `auto_commit_block_on_english`（默认关）——满码上屏时合并结果存在英文候选则否决
（保护正在输入更长英文词的用户）。
**空码清空**（第四条「拼音让路」通路）：主码表请求清空后，再过两道拼音守护——
`has_pinyin`（此刻已出拼音候选）与 `pinyin_may_continue`（`is_possible_pinyin_sequence`：
整串是合法音节前缀，或完整音节 + 合法尾部前缀，如 zhon→zhong 的中途态）。

两道**同受 `auto_commit_block_on_pinyin` 支配**，与上三条通路同一个开关：

```rust
should_clear = ct_should_clear && !(auto_commit_block_on_pinyin && (has_pinyin || pinyin_may_continue))
```

⚠️ 两道必须一起受控，只放开 `has_pinyin` 等于没放开：`nunl` 这类「完整音节 + 单个声母字母」
即便词库无候选，`pinyin_may_continue` 仍判「还没打完」（单字母恒是某音节前缀）而独立拦住清空。

**清空还要过第三道门**（协调器 `clear_blocked_by_candidates`，见 §8）——引擎在追加短语之前
就算好了 `should_clear`，且它只按音节表推测「还有没有后续」，而协调器看得到候选的实际形态：

| 输入 | 拼音候选 | code | consumed | 判定 |
|---|---|---|---|---|
| `nunl` | 嫩 | `nun`（比输入**短**） | 3 < 4 | 部分匹配 → 放行清空 |
| `wanl` | 完了/晚了 | `wanle`（比输入**长**） | 4 = 4 | 前缀补全 → 拦住清空 |

故关闭开关**不牺牲**「还没打完」的中途态：`wanl`/`zhon` 由前缀补全候选兜住，真正被清空的
只有「候选全是部分匹配」的串。

### 7.4 超长分支（`convert_overflow()`，engine.rs:265-349）

`input_len > max_code_len` 时按 `pinyin_only_overflow` 分流（config.toml 默认 true）：

- **true（默认）**：仅查拼音 + 英文。两个互补的码表回捞口，任一成立即把码表候选并回来
  （拼音同时归一化降档）：
  - 长码特例 `has_full_input_match(input) || has_longer_code(input)`：问的是**整串**，只有码长
    可变的码表够得着 —— 五笔这类定长码表恒假（4 码封顶，不存在 5 码词条）。
  - `codetable_owns_overflow(input)`：问的是**前 N 码**，这才是定长码表的逃生口，与顶码 ⓪ 共用。
- **false**：码表取前 N 码（+ 长码特例的整串候选）+ 拼音整串，统一加权混合竞争。

#### `codetable_owns_overflow` 的四条判据（缺一不可）

| # | 判据 | 拦住的场景 |
|---|---|---|
| 1 | 前 N 码前缀是码表**精确全码** | 拼音打错一个字母（`nihxo`：`nihx` 无全码）不被五笔顶码截胡 |
| 2 | 拼音**主张不了**整串（`pinyin_claims_overflow`） | `youyo` = you + `yo` 还没打完 ⇒ 归拼音（`youyoud`→「变凉」回归） |
| 3 | 英文**主张不了**整串（`english_claims_overflow`：有精确整串词条） | 开着英文词库时 `words` 首选被码表 `+1e7` 压掉英文 `+500K` |
| 4 | 拼音至少**交得出候选**（`pinyin_has_any`） | `github`：连开头都读不出一个字，判给码表毫无依据 |

前三条问的都是「谁解释得了整串」，第 4 条问的是「这串还算不算中文」，别混为一类。第 2 与第 4 条
方向相反，两头夹出「还在中文语境里、但拼音接管不了整串」这个窄带 —— `yijga` 出得来「以」却只
解释 2/5，正落在带内。

判据 3、4 命中时候选保持为空（无英文库的情况下），用户空格/回车直接上屏原码，这是 249f486
之前的行为。判据 4 对**顶码通路无影响**：⓪ 是 `pinyin_only_overflow && has_pinyin && !ct_owns`，
`has_pinyin=false` 时整条本就不成立；顶码侧的英文场景另由 ③ `auto_commit_block_on_english` 管。

★ **这个归属结论由 ⓪ 与 ① 共用**（`handle_top_code` 里求值一次，两处同读）。曾有一版只豁免 ⓪，
于是 `cety`+第 5 键在**出厂配置**下候选侧回捞了「通往」、顶码侧却被 ① 拦死 —— 而 ⓪ 对应的设置项
「超码长时仅查拼音」是 `hidden`，用户在设置页里一个开关都关不掉。修正后 ① 同享豁免，② 不享。

⚠️ **简拼是这条例外口的已知边界**：`enable_pinyin_abbrev`（出厂**开**）时，简拼整句候选的
`consumed_length` 覆盖整串 ⇒ 判据 2 `pinyin_claims_overflow` 成立 ⇒ 例外口不成立 ⇒ ⓪ 独立拦下
顶码。这是**有意保留**的取舍：简拼几乎能主张任何字母串，若让它不算「主张」，长简拼词
（`zgrmghg` 之类）会被前 4 码的五笔全码顶码截胡。取舍为「习惯混输顶码的用户多半关简拼，
不牺牲简拼用户」。改这条前先重估两类用户的碰撞面。

回捞的前缀候选有两处归一化，都因为它**只解释得了前 N 码**：
- `is_exact_code = false`（下游一律以完整输入为准，见 §7.5 / `freq_tier`）；
- `consumed_length = 前 N 码长度` —— **码表候选带 consumed_length 的唯一出口**。不标的话协调器
  `commit_selected` 的 `partial = consumed > 0 && consumed < total` 恒为 false ⇒ 按「消费整串」
  上屏，选中即把尾码吃掉（`yijga` 选「就是」→ `a` 消失）。协调器侧两处依赖「码表恒 0」的判据
  已随之对齐，见 §7.6 分段上屏接力与 `learn_phrase_on_commit`。

### 7.5 顶码/满码上屏与显示一致（协调器侧配合）

引擎层否决之外，协调器保证「**上屏即所见、非码表来源不上屏**」：

- **满码**（handle_candidate.rs:322-334）：自动上屏文本取**实际显示的首候选**（与空格/点选同源），
  且**仅当显示首选 source==CodeTable** 才上屏——若首选被 shadow 置顶为拼音、或码表精确字被智能过滤后
  仅剩拼音，则放弃自动上屏留给用户选。
- **顶码**（coordinator.rs:3413-3453）：字母键入前先记住「即将成为前缀」的缓冲及其**显示首选**
  （已过滤/重排/shadow 的用户所见），仅当其为码表来源时作为顶码文本；显示首选非码表 → 放弃顶码继续组合。
  多级溢出（前缀≠顶码前缓冲的罕见场景）才回退引擎顶码文本。
  背景：调频置顶/shadow 发生在协调器层，引擎 `handle_top_code` 内部 convert 看不到，会顶出权重首选
  而非显示首选。

### 7.5b 拼音候选须消费整串（`pinyin_partial_candidates{,_overflow}`）

混输下拼音会把**只解释了输入开头一截**的候选也交出来：`gedw`（五笔精确全码「青春」）里
`ge` 是合法音节、`dw` 连音节前缀都不是，于是 `code=ge`、`consumed_length=2` 的同音单字
**实测 219 条**（真实词库，占 226 条候选的绝大多数）。主流混输实现均不出这类候选。

- 判据：`consumed_length == 0 || >= input.len()`（0 = 未标注 ⇒ 按整串算）。
  **不可用 `!is_partial` 代替**——Viterbi 整句走 `insert(0)` 不经算 `is_partial` 的闭包
  （`aaw`→「啊啊」consumed=2 而 `is_partial=false`）。
- 落点：`Engine::convert_with_opts()` 的 `ConvertOptions::require_full_match`，过滤在**拼音引擎
  内部、排序 `truncate` 之前**。
  ⚠️ 由调用方拿结果再 `retain` 是错的：简拼候选在 `cmp_match_layers` 里最沉，配额被残码占满时
  它在截断那一步就没了（实测混合简拼「各单位」被压到第 221 位，滤掉残码后回到第 2 位）。
- 两档独立开关：码长内默认**关**（丢弃），超码长默认**开**（保留）——后者已切入纯拼音语境，
  长拼音的分段上屏要留着。做成参数而非 `PinyinConfig` 字段，是因为两条路径共用同一个子引擎实例。
- **前缀补全不受影响**：`wanl`→「完了」的 code 是 `wanle`（比输入长）⇒ 消费整串，实测过滤后
  `wanl` 仍有 151 条候选。判据切在「解释完整度」而非「候选类型」上，正是为此。
- 连带：`has_pinyin` 随之转假 ⇒ §7.3 的拼音否决①与满码空码清空守护在这类输入下不再拦截
  （`pinyin_may_continue` 那道仍在）。顺带根治 `nunl` 出「嫩」的老问题。

### 7.5c 残码整句的作用域：超码长开、码长内关（`ConvertOptions::allow_partial_final`）

step 2c（尾部残码参与整句解码）此前在混输下**整体关闭**（`manager.rs` 的
`enable_partial_final: mix_pinyin.is_none()`）。代价：`zaiyebuj` 的尾字母 `j` 不参与组句，
一条**消费整串**的候选都没有（首选「在也不」只解释 7/8 键，上屏后 `j` 留在缓冲里），
而纯拼音方案打得出「在也不就」——主流实现均以「优先匹配输入的音节」为准。

关闭的理由（真机 `aaw`，本意五笔 `aawt`→「工作」，整句「啊啊我」消费满 3/3 键后合法跨过
`is_pinyin_exact_tier` 的闸门抢走首位）**只在码长内成立**：定长码表（五笔 4 码）之外的串
不可能是码表码。⇒ 判据从「是不是混输」改为「**这串还可能是码表码吗**」，两条路径各自取值：

| | 码长内 | 超码长 |
|---|---|---|
| `require_full_match` | 随 `pinyin_partial_candidates`（出厂丢弃） | 随 `..._overflow`（出厂保留） |
| `allow_partial_final` | 强制 `false` | 强制 `true` |

落点 `MixedEngine::{in_code_len_opts, overflow_opts}`——两侧各写一个方法而不是传 bool，
是为了让「同一维度在两侧取值相反」这件事在源码里看得见。`manager.rs` 那行**保持 false**
（默认关、由调用方覆写），使任何未覆写的调用点行为不变；混输里三处判据函数
（`is_ambiguous_pinyin_word` / `pinyin_claims_overflow` / `pinyin_has_any`）刻意仍走 `convert`。

实测（协调器层显示序）：`zaiyebuj` 首选变为「在也不就」、`buzhidaok` 首选变为「不知道看」，
`aaw`／`gedw` 一字未变。

### 7.6 混输其它

- **来源提示**：`show_source_hint`（默认关）给拼音候选 comment 加「拼」前缀（`add_source_hints()`）。
- **preedit**：拼音解析出 ≥2 完成音节时组合区用音节分隔串（ni'hao），否则原始码（单音节/纯五笔码不拆）。
- **分段上屏接力**（`build_candidates`）：**最后一段来自拼音选词**时，剩余编码**强制**按混输方案的
  `[engine.mixed].secondary_schema` 转换（`convert_with`），避免混输让码表抢首选（选「你」后 hao→虚）。
  注意不用全局 primary_pinyin（那是临时拼音↔临时双拼切换用的）。
  ⚠️ 判据曾是「`committed_text` 非空」，理由是「必来自拼音选词——码表候选 consumed_length=0 永不
  部分匹配」。**该等价关系已不成立**：§7.4 的超码长回捞候选如实标注 consumed_length，码表也会进入
  分段态；沿用旧判据会让 `yijga` 选「就是」后剩下的 `a` 被强制按拼音解释。同源的
  `learn_phrase_on_commit`（拼音专属造词）也据此对全段码表显式跳过 —— 码表侧造词归 `auto_phrase`
  连续单字缓冲管，且各段码机械拼接（`yijg`+`a`）本就不是有意义的词条。
- **临时拼音**：`[input.temp_pinyin]`（总开关 + 引导键，默认反引号）由协调器 pipeline 层分发，临时切到
  目标拼音方案，不在 MixedEngine 内。目标方案取全局 `schema.primary_pinyin`（空=全拼 `"pinyin"`），
  见 `temp_pinyin_target`。
- **快捷输入**：`;` 由内置 mix「快捷」（`quick_mix`）融合接管，各候选来源即其 `members` 成员——
  `quick_input.calc`（算式）/ `.date`（日期年月）/ `.number`（数字金额）/ `.repeat`（重复上屏）
  与 `$primary_pinyin` / `english`。**有无即开关、顺序即优先级**，无旁路 bool 开关
  （旧的 `schema.quick_input.enable_english` 已废弃并在加载期迁移为成员删除）。
  也没有总开关——禁用即把 `trigger_keys` 清空（曾有 `quick_input.enabled`，但它从未被任何
  逻辑读取，关掉不产生任何效果，已删）。
  前三者由 `wind-quick-input` 纯函数产出，`.repeat` 取 `recent_commits` 上屏历史（仅空缓冲时）。
  透镜分派见 `mix_has_quick_numeric`：含表达式类来源才开数字透镜（`-`/`=` 作运算符而非翻页）。
- **词库热插拔**：`set_dict_enabled` 转发主/次子引擎（扩展码表层在码表子引擎）。

---

## 8. 候选后处理管线（协调器层，所有模式统一）

`build_candidates()`（`wind-coordinator/src/handle_candidate.rs:177-340`），引擎返回后依次：

```
① engine convert（初始 limit 按引擎类型/码长阶梯，:160-171：码表 100/300/1000，拼音/混输 300）
② 短语注入（wind-phrase lookup + lookup_prefix）：
     静态/模板短语、$CC 命令（is_command）、$SS/$AA 组（is_group 二级展开）
     weight = hit.weight（精确码/前缀枚举同口径；曾有的 PHRASE_WEIGHT_BASE=40M 类别硬顶已删除，
     短语按自身权重与码表精确候选竞争，见 candidate-sorting-rules.md §5.1）
③ 层级排序：is_fuzzy asc → **is_partial asc** → is_prefix asc → weight desc → natural_order asc
     （Fuzzy＜子短语＜前缀补全＜完整匹配：与 PinyinEngine 内部排序一致。缺 `is_partial` 时，
     高权重子串单字会靠 weight 反超低权重精确词组——如 `pingtan` 下
     平(w=58 part=true)＞平摊(w=4 part=false)，前者插到词组前）
④ 按 text 去重（保留首个）+ **把被弃条目所占码位并入幸存者**（merged_codes，见 §8.1.2）
⑤ apply_filter：填充 is_common（常用字表；短语豁免，判定作用域见 §8.1.1）→ wind_candidate::filter_candidates
⑥ apply_freq_rerank：用户词频重排（独立维度，绝不改 weight）
⑦ apply_shadow：shadow 规则删除过滤 + 置顶/移动重排（优先级最高，排序后应用）
⑧ 自动上屏复评与守护：
     引擎意向 or recheck_auto_commit（显示态复评，惰性）
     → 目标须在最终候选中存活（未被 shadow 删除）
     → 显示首选须 source==CodeTable 才 AutoCommit（§7.5）
```

### 8.1 智能过滤：按 (source, code) 分组

`wind-candidate/src/filter.rs`。`FilterMode`：`Gb18030`（不过滤）/ `General`（仅常用）/
`Smart`（智能）。Smart 规则：**按 `(CandidateSource, code)` 分组**，同组内存在常用词
（is_common/is_phrase/is_command/is_group）则滤掉非常用，无常用则整组保留。
按来源分组是提交 19d580f 的修复：混输下码表与拼音候选常共用同一 code 串（如 wang），
原先只按 code 分组会让拼音常用字误杀同码的码表生僻字（佢），导致混输码表表现与纯五笔不一致。

#### 8.1.1 常用性判定的作用域（`wind-candidate/src/common.rs`）

`is_string_common` 逐字判定：**属「汉字」的字符必须在通用规范汉字表（8105 字）内，其余辅助
字符忽略**。所谓「汉字」= `is_han` ∪ `is_pua`，两侧各挡一类误判：

- **纳入 PUA**（158a383）：码表把私用区码位当汉字使用（`dwi` 下 U+E831 冒充生僻字、占着
  汉字编码排在「仄」旁边），不查表就会让无字形的豆腐候选混进「常用字/智能」档。
- **`is_han` 排除 CJK 标点与符号**：`、。《》〈〉「」〇`（U+3000–U+303F）、假名、注音、
  谚文、带圈与兼容符号（`① ㈱ ℃ ㎡`，U+3200–U+33FF）虽紧邻汉字块，却与 `，`(U+FF0C)、
  emoji 同属辅助符号，规范汉字表对其无从判断。旧实现按整段 `0x2E80..=0x33FF` 圈定判定域
  （沿用 Go `isCJKChar` 的名字与范围），把它们当成必须查表的汉字 → 用户词库里含中文顿号的
  词条在「常用字/智能」档被静默滤掉。**该缺陷的指纹是判定不自洽**：同为中文标点，`、` 判
  非常用而 `，` 判常用，差别只在落没落进那段区间。

判据是语义而非 Unicode 块邻接：**「码表拿它当汉字用」才查表，「它只是符号」就忽略**。
部首（U+2E80–U+2FDF）与笔画（U+31C0–U+31EF）保留在判定域内，理由同 PUA——它们无独立输入
语义，却会占着汉字编码出现在候选里。

> **待办（未实施，用户已确认方向）**：给「用户词库词条不受检索范围过滤」加一个开关。
> 现状是用户显式加进词库的生僻字（`囍 靐 嘅 冇` 等均不在规范字表）在「常用字」档仍被滤掉，
> 只能整体切到 `gb18030` 档才打得出，与「我自己加的词就该打得出」的直觉相悖。
> 落点：`handle_candidate.rs` `apply_filter` 处 `c.meta.is_user_dict` 已由 dict 层
> （`wind-dict/src/store_layer.rs:22-26`）置位，可直接读，形如
> `c.is_common = c.meta.is_user_dict || self.common_chars.is_string_common(&c.text)`。
> **豁免范围须止于 `is_user_dict`**：`is_temp_dict` 是码表自动造词的产物（连续单字+终止符
> 自动成词，杂词率高），一并豁免等于让自动造出的杂词绕过这层过滤，而那正是它该管的。

#### 8.1.2 分组键的完整性：去重不得吃掉码位（`Candidate::merged_codes`）

分组按 `code`，但**去重跑在过滤之前**，会把同一个字在别的码位上的条目整条丢掉——那个码位
的常用性归属随之消失，过滤结果因此**不单调**：

- 五笔「桜」(sivg)：`sivg` 码位下另有常用字「档」，而「档」还有简码 `siv`；
- 打 `siv` → 「档」以 `code="siv"` 入列，它在 `sivg` 的那条被去重丢弃 → `sivg` 组只剩「桜」
  成孤儿码而**放行**；
- 打全 `sivg` → 「档」「桜」同组 → 「桜」被滤。**同一个字，打得越全反而越不出**。

修法：去重时调用 `absorb_codes_from`，把被弃条目的 `code`（及它自己早先归并的码位——去重是
链式的）并入幸存者的 `merged_codes`；`filter_smart` 统计时，常用候选同时遮蔽自身 code 与
merged_codes。**当前四个归并点**：`composite::merge_search`（跨词库层）、`CodeTableEngine::convert`
的精确/前缀两循环、`MixedEngine::sort_dedup_truncate`、协调器 `build_candidates`。

两条边界：

- **跨来源一律不并**（`absorb_codes_from` 内置守卫）：码表码与拼音码不同域，混输下 "wang"
  两边都合法；并入会给码表凭空造出「该码位有常用字」的假事实，误滤同码的码表生僻字——
  正是 §8.1 按来源分组所要避免的那个缺陷的对称形态。
- **跨方案合并不接**（快捷输入 `handle_mode` 按 mix members 汇总）：不同方案的码同样不同域，
  而它们的 `source` 可能同为 `CodeTable`，守卫拦不住；该路径本身也不经过检索范围过滤。

⚠️ `CodeTableEngine::convert` 里 `source` 必须**先于** `absorb_codes_from` 赋值——守卫跨来源
直接 return，而 `dm` 返回的候选 `source` 尚为 `None`，晚一步赋值会让归并**静默失效**。

> 本次只统一了行为，**未改变智能档的产品语义**：「桜」这类唯一编码被常用字占着的生僻字，
> 在智能档下仍然打不出，需切「全部字符」(`gb18030`) 档。放宽规则（如全码精确命中豁免过滤）
> 属后续优化，且须先清理词库里残留的 79 条 PUA 垃圾条目——它们目前正靠 is_common=false 被挡着。

### 8.2 词频重排（freq_rerank.rs，两策略）

不修改 weight，是排序后的独立重排维度（词频数据在 wind-store，按 §1.3 的 schema id 分键空间；
仅 `consumed_length==0` 或 ≥ 输入长的候选参与取频）：

- **码表/混输：永久 used-first**（`rerank_codetable_usedfirst()` :46-91）。
  档位感知（`freq_tier()` :26-38）：0=码表精确全码、1=短语、2=码表前缀等、3=拼音/英文；
  **同档内** used-first（策略 `Top`=MRU / `Step`=按 count），**跨档永不反超**；
  重排前记录前 N 位、重排后回填的 `protect_top_n` 呈现保护。
- **拼音：衰减软置前**（`rerank_pinyin_decay()` :99-170）。
  整句豁免（weight ≥ PINYIN_SENTENCE_FLOOR 20M 恒顶）；层级保护（模糊<精确、补全<精确、
  子短语<完整，词频不得跨层反超）；衰减分 < 阈值则褪色失去置前资格（半衰期等参数
  `schema.pinyin.frequency.*`）。

### 8.3 Shadow 规则

`wind-candidate/src/shadow.rs`：用户「删词/置顶/移动」规则，删除过滤 + 按目标位置重排，
在过滤与词频之后应用（最高优先级）；自动上屏目标被 shadow 删除则不放行（⑧步存活复核）。

---

## 9. 辅助码模式（AuxCode）—— 拼音候选的字形二次筛选

协调器 overlay 模式（`ModeKind::AuxCode`），**非引擎**：对已跑完 §8 管线的拼音候选做字形二次
筛选。文件：`wind-coordinator/src/handle_aux_code.rs` + `wind-aux-code/`（纯逻辑：三段式紧凑表
+ 纯筛选，不碰文件系统，路径由调用方解析）。

- **配置（三层，同 `[schema.codetable]` 那套 tri-state，见 schema-config-layering.md §4）**：
  全局基线 `[schema.pinyin.aux_code]`（`enabled` **出厂 false** / `max_phrase_len`）；
  方案段 `[engine.aux_code]` 放 `files`（方案属性：全拼配笔画、双拼配小鹤形码），并可用
  同名 `enabled` / `max_phrase_len` 逐字段覆盖全局；`schema_overrides/{id}.toml` 用**相同段名**
  经 `read_schema` 的 `merge_toml` 深合并（设置页写入点）。折叠在
  `AuxCodeGlobal::resolved`，取值出口只有 `EngineManager::aux_code_settings` 一个。
  ★ **`files` 非空 ≠ 功能开启**：方案配码表只是「这个方案推荐哪张表」。
- **★ 为什么 `enabled` 必须能按方案覆盖**：全拼与双拼在这个功能上不是偏好不同，是**键位
  预算不同**。双拼把韵母塞进字母键、符号键全空闲（`pinyin_separator_key` 对双拼恒早退）；
  全拼的音节边界必须靠符号表达，出厂 `separator = "auto"` + `'` 作选词键 ⇒ **反引号即音节
  分隔符**。且按键 match 里分隔符臂（`message_handler.rs` 的 `VK_QUOTE|VK_BACKTICK`）位于
   `[session_actions]` 裁决**之前**（分隔符判定在按键分发最靠前），所以全拼下哪怕把反引号绑成
   `session_actions.aux_code` 也永远走不到辅助码。于是「双拼开、全拼关」是常态需求，一个全局开关表达不了。
- **触发**：`session_actions` 绑 `"aux_code"`（或 `"page_next_aux_code"` 共键）；双拼出厂绑反引号
   （绑定本身不激活功能，`enabled = false` 时门卫拒绝、该键照常落普通标点），全拼出厂不绑（理由见上）。
  门卫四道：未启用 / 触发键被音节分隔符占用（`warn_aux_code_key_taken` 每方案告警一次，
  不再静默失效）/ 方案未配 `files` 或文件全缺 / 当前无候选 → 一律不吞键。
  组码中进入，**只筛选不改排序**。
- **加载**：`EngineManager::aux_code_settings` 一次 `read_schema` 出齐 `enabled` /
  `max_phrase_len` / 已解析的 `files`（用户目录同名优先；**关闭时不解析路径**）；首进时
  `ensure_aux_code_table` 懒加载 + `merge`（先出现 = 高优），**不参与预热**。缓存是全局一份，
  各方案码表不同（拼音笔画表 vs 双拼小鹤全码表），**切方案必须失效重挂**
  （`invalidate_aux_code_table`，随 `sync_chaizi_assets`/`sync_comment_dicts` 一起）。表格式：
  UTF-8 `字=码` 一行一条（`=` 分隔，与 rime-lua-aux-code `aux_code` 目录一致），`#` 注释跳过，第 1 行可选 `# name:`（缺省回落文件主干名），
  version/source 当普通注释不解析。码表文件是 `wind-tools/gen_aux_code` 的构建产物、
  **不入版本库**（rime-stroke 为 LGPL-3.0，见 NOTICE.md）。
- **筛选**（`wind-aux-code/src/filter.rs`）：`aux_code_matches` 谓词判断单个候选是否匹配；
  `filter_by_aux_code` 批量筛选，`kept` 是输入候选的**子序列**（不变量，绝不重排）；
  单字任一码前缀匹配 `aux_input` 则留；词组**逐字首码匹配**（固定语义，无模式选项）——第 i 位
  命中第 i 字任一码的首字符，恰好 N 位，**不足 N 位 = 前缀态保留（边打边缩）、超过或某位不中
  都滤**；字符一律按表查询，不做纯汉字判断；
  空输入 / 空表 = **不过滤**（防候选窗滤光）。
- **会话**（`AuxCodeSession`）：内部持 `CandidateStore`（原始候选快照 + 筛选视图）+
  辅助码缓冲；每次从快照**重筛**（通过 `CandidateStore::set_filter`，否则退格还原会乱序）；
  **被滤候选直接丢弃**——辅助码是字形二次筛选，候选窗只显示命中者（如 `om` 配「时间」
  「实践」时实践消失），还原不靠残留标记，退出/退格都从快照恢复（`CandidateStore::clear_filter`）。
  **显示态**（组合区/光标）与筛选会话一起打包在协调器 `State.aux_code: Option<AuxCodeOverlay>`
  （`preedit_base` 退出还原、`preedit_prefix` 进入时拼一次 = 拼音基线 + 4 空格），三件套
  同生共死、整体销毁；刷新组合区与 overlay 光标共用同一前缀，分隔符只写一遍。组合区
  = 显示前缀 + 辅助码。
- **键语义**：字母累积（小写化）；Esc 退出还原拼音；Backspace 删空保持空码态、已空再退格退出；
  Space/Enter/数字选词上屏后退出；翻页/高亮走统一 `handle_candidate_nav`；Ctrl/Alt 组合走公共
  `overlay_ctrl_alt_guard`。退出 `exit_aux_code` 刻意不 `ClearComposition`（筛选非放弃组合）。
- **表名**：`mode_indicator_names` 的 AuxCode 分支读表 `name`（如「笔画」）显示在指示位，
  未命名/未加载则沿用主路径。

---

## 10. 各模式流程对比

| 阶段 | 码表 | 拼音（全拼） | 双拼 | 混输 | 英文 |
|---|---|---|---|---|---|
| 词库 | codetable 多层（主+扩展+用户+临时） | rime_pinyin merged.wdb + 用户/临时层 | 同拼音 | 主码表全套 + 拼音全套 + 可选英文 | 码表格式，code 小写化 |
| 输入预处理 | 无（原始码） | `'` 分段 + DAG 音节切分 + 模糊音扩展 | **先双拼→全拼**（Layout 表 + 位置映射），后同全拼 | 双路各自原样进子引擎 | 小写化 |
| 候选生成 | 精确 + 前缀 + 空码补全 | 六步：精确/Viterbi 整句/子短语/前缀/简拼/用户层 | 同全拼 | 码表全流程 + 拼音全流程 + 英文，档位加权合并 | 精确 + 前缀 |
| 引擎内排序 | better()（weight 主导） | 层级（模糊/前缀/子短语）→ weight | 同全拼 | 截断档（码表精确 > 短语 > 码表前缀/英文整词 > 拼音/英文前缀），**档内保持子引擎原序** | weight |
| 自动上屏 | 满码唯一精确且无更长后继 | 无 | 无 | 码表意向 + **拼音否决①② + 英文守护 + 存活复核 + 显示首选须码表** | 无 |
| 顶码 | 超满码顶前 N 码首选，余码续打 | 无 | 无 | 同否决①②后委托码表；`top_code_override_pinyin` 可强制 | 无 |
| 分段上屏 | 无（consumed_length=0） | consumed_length 前缀消费，余码续转 | 同全拼（映射回双拼键数） | 拼音候选支持；接力强制走 secondary_schema | 无 |
| 空码行为 | 满码空码清空（可配） | 不清空 | 不清空 | 三道门：码表请求清空 → 两道拼音守护（受 `auto_commit_block_on_pinyin` 支配）→ 协调器候选复核（拼音部分匹配不算有效候选） | — |
| 词频重排 | used-first 永久档位 | 衰减软置前 | 衰减软置前 | 按候选 source 分流两策略 | 归入档位 3 |
| preedit | 原始码 | 音节 `'` 分隔 | 原始按键按音节分隔 | `preedit_display`：≥2 音节用拼音拆分串，否则原始码。`preedit_pinyin`（高亮跟随用）判据更宽：**拆分串 ≠ 原串**即给出，覆盖单音节 + 残码（`nun'l`） | 原始输入 |

后处理管线（短语/过滤/词频/shadow/复评，§8）对所有模式统一，由协调器执行。
辅助码（§9）不在此表——它不是引擎，是协调器 overlay 模式，对已跑完管线的拼音候选做字形二次筛选。

---

## 11. 配置速查（实际生效默认值）

引擎相关配置三层合并：代码默认 → 系统 `data/config.toml` → 用户 `%APPDATA%\WindInput\config.toml`。
下表「默认」为**系统配置层实际值**（与代码默认不同处已注明）：

| 键 | 默认 | 说明 |
|---|---|---|
| `schema.mix.auto_commit_block_on_pinyin` | true（三处同源） | 否决① 粗粒度：有拼音候选即否决上屏；**兼管满码空码清空的两道守护** |
| `schema.mix.block_commit_on_pinyin_word` | true | 否决② 拼音词拦截（实际主力） |
| `schema.mix.pinyin_word_min_weight` | 0 | 0=仅结构判据（≥2 汉字消费整串） |
| `schema.mix.pinyin_partial_candidates` | false（三处同源） | 码长内是否保留「未消费整串」的拼音候选（`gedw` 的 219 条 `ge` 同音字），见 §7.5b |
| `schema.mix.pinyin_partial_candidates_overflow` | true（三处同源） | 超码长同上；默认保留，长拼音的分段上屏靠它 |
| `schema.mix.top_code_override_pinyin` | false | 顶码优先，无视拼音否决 |
| `schema.mix.pinyin_only_overflow` | true | 超码长仅拼音+英文 |
| `schema.mix.min_pinyin_length` | 2 | 拼音最小触发长 |
| `schema.mix.enable_english` / `min_english_length` / `auto_commit_block_on_english` | false / 3 / false | 英文混入及守护 |
| `schema.mix.show_source_hint` | false | 拼音候选「拼」标记 |
| `schema.codetable.*`（auto_commit_at_full / auto_commit_min_len / clear_on_empty_max / top_code_commit / show_code_hint / single_code_input / single_code_complete 等） | 见 config.toml | 可被 `schema_overrides/{id}.toml [codetable]` 按方案覆盖 |
| `schema.pinyin.use_smart_compose` | — | Viterbi 整句开关 |
| `schema.pinyin.fuzzy.*` | — | 模糊音 11 对开关 |
| `schema.pinyin.frequency.*`（half_life / base_scale / recency_peak） | — | 拼音词频衰减参数 |
| `schema.pinyin.auto_learn.*` | — | 自动造词 |
| `[engine.pinyin] scheme` / `[engine.pinyin.shuangpin] layout` | quanpin / — | 双拼判定与布局 |
| `[input.temp_pinyin]` enabled/schema/trigger_keys | true / pinyin / backtick | 临时拼音引导 |

---

## 12. 关键源文件索引

| 模块 | 文件 |
|---|---|
| 统一入口/构建/热重载 | `wind-engine/src/manager.rs` |
| Engine trait / ConvertResult | `wind-engine/src/engine.rs` |
| 码表引擎 | `wind-engine/src/codetable/engine.rs` |
| 拼音引擎 | `wind-engine/src/pinyin/mod.rs`（+ syllable/dag/fuzzy/lattice/viterbi/lm/scorer/generate） |
| 双拼转换 | `wind-engine/src/pinyin/shuangpin.rs`；布局 `data/schemas/shuangpin/*.toml` |
| 混输引擎（否决①②/档位/overflow） | `wind-engine/src/mixed/engine.rs` |
| 英文引擎 | `wind-engine/src/english.rs` |
| 词频重排 | `wind-engine/src/freq_rerank.rs` |
| Candidate / 智能过滤 / shadow | `wind-candidate/src/{candidate,filter,shadow}.rs` |
| 词库（格式/缓存/多层） | `wind-dict/src/{codetable,binformat,datformat,cached,composite,manager,layer,store_layer}.rs` |
| 后处理管线 / 顶码守护 | `wind-coordinator/src/handle_candidate.rs`、`coordinator.rs`（VK_A..Z 顶码段） |
| 辅助码（overlay 模式 / 纯筛选 / 三段式表 / 表格式） | `wind-coordinator/src/handle_aux_code.rs`、`wind-aux-code/src/{table,loader,filter}.rs`、`data/schemas/aux_code/*.txt` |
| 配置结构与注册 | `wind-config/src/{config,schema,config_schema}.rs`、`data/config.toml` |
