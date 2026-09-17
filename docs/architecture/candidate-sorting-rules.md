# 候选排序规则（权威）

> **本文件是候选排序的唯一真相（source of truth）。** 凡改动任何与「候选先后顺序」有关的
> 代码——引擎内排序、`candidate_display_order`、`freq_rerank`、混输加权、短语注入、shadow——
> **必须先读本文件、改完后回来对齐本文件**。三套排序系统各改一处极易漂移（历史上反复发生），
> 本文件的存在就是为了把它们钉在同一口径上。
>
> 涉及文件：
> - `wind_input/crates/wind-candidate/src/candidate.rs`（字段定义 + `cmp_match_layers`/`cmp_exact_first`/`better`/`by_natural`）
> - `wind_input/crates/wind-engine/src/codetable/engine.rs`、`pinyin/mod.rs`、`mixed/engine.rs`（引擎内排序）
> - `wind_input/crates/wind-engine/src/freq_rerank.rs`（词频重排两条路径）
> - `wind_input/crates/wind-coordinator/src/handle_candidate.rs`（`candidate_display_order` + 装配流水线）

---

## 0. 一句话心智模型

一条候选从产生到显示，要穿过**三套彼此独立的排序系统**，后一套可以整体推翻前一套：

```
引擎内排序  ──►  协调器 candidate_display_order  ──►  词频重排 freq_rerank  ──►  shadow
（各引擎自排）    （无条件重排全部候选，匹配层为主）      （开自动调频且有记录时，          （置顶/删除，
                                                        首要键整体压过上一步）           最高优先级）
```

**最容易踩的坑：** 你以为改 `candidate_display_order` 就能决定最终顺序——**错**。只要开了自动调频
且有词频记录，`freq_rerank` 的档位/锚定是**首要键**，会整体压过 `candidate_display_order`
（`freq_rerank.rs` 头部注释、`candidate.rs` `cmp_exact_first` 文档都有警告）。**验证「匹配层/精确
优先」类改动必须先关自动调频**，否则会误判「改动没生效」。

---

## 1. 完整流水线（`Coordinator::build_candidates`，`handle_candidate.rs`）

按执行顺序，一次候选刷新经过以下步骤：

| # | 步骤 | 位置 | 作用 |
|---|---|---|---|
| 1 | **引擎 `convert`** | `engine_mgr.convert*` | 各引擎产出候选并**引擎内排序**（见 §4） |
| 2 | **`finalize_candidates`** | `handle_candidate.rs` | 词库值内嵌 `$CC/$AA/$SS` 展开（打 `is_command`/`is_group`，**不打 `is_phrase`**） |
| 3 | **短语注入** | 同上，`phrases.lookup` + `lookup_prefix` | 全局短语进候选（打 `is_phrase`，见 §5） |
| 4 | **空码补全收口** | 同上 | 精确模式无候选时补一条（`completion_hint`/`completion_pool`，统一判空取一条） |
| 4.5 | **`mark_common`（常用字判定）** | `handle_candidate.rs` | 无条件填 `is_common`（**不看 `filter_mode`**）；混输拼音精确档拿它当提档准入，见 §6 ③ |
| 5 | **`candidate_display_order` 排序** | 同上 | **无条件重排全部候选**（七级链，见 §6） |
| 6 | **按 `text` 去重** | `retain(seen.insert)` | 保留排序后第一条 |
| 7 | **`apply_filter`** | `handle_candidate.rs` | 检索范围过滤（常用字/GB18030）；**短语恒保留**（`is_common` 已由第 4.5 步填好）；含生僻字的**多字候选**另由 `input.rare_phrase` 决定（出厂整词放行，见 §9） |
| 8 | **`apply_freq_rerank`** | 同上 | 开自动调频**且有词频记录**时重排（见 §7）——**首要键压过第 5 步** |
| 9 | **`apply_shadow`** | 同上 | 用户 shadow 规则：删除 + 置顶到指定位（**最高优先级**，见 §8） |
| 9.5 | **`short_code_yield::apply`** | `short_code_yield.rs` | 出简让全：有简码的字在更长码位上把首选让给词并沉底。**排在 shadow 之后**（第 8 步的硬约束所迫），故对「用户排过序的码」整码停手，见 §8 |
| 10 | **自动上屏复核** | 同上 | 满码/顶码唯一自动上屏判定（不改顺序，只决定要不要上屏） |
| 11 | **`expand_s2t_variants`** | 同上 | 简繁 1对多变体紧跟原字插入（**必须在去重/重排/shadow 之后**，见 §9） |

> ⚠️ 第 5 步与第 8 步的先后是 `freq_rerank` 正确性的前提：`freq_rerank` 的「维持权重序」分支
> 靠**稳定排序**保住第 5 步喂进来的顺序，它自己**从不比 weight**。调换两步、或在中间插重排，
> `freq_rerank` 的输出即失去权重语义且不报错（`freq_rerank.rs` 头部有详述）。

---

## 2. 候选的排序相关字段（`Candidate`，`candidate.rs`）

| 字段 | 类型 | 语义 | 谁置位 |
|---|---|---|---|
| `is_fuzzy` | bool | 模糊音变体命中（非原拼音精确） | 拼音引擎 |
| `is_prefix` | bool | **前缀补全**（候选码比输入长，如 `si`→思考`sikao`）；**又被全局短语借作「非精确层」标记** | 拼音引擎；协调器前缀短语（`lookup_prefix`，`is_prefix=!codetable_mode`） |
| `is_partial` | bool | **子短语**（候选码是输入的真前缀、比输入短，如 `baoan`→报`bao`） | 拼音引擎 |
| `is_exact_code` | bool | **精确匹配档**（候选 `code`==输入的完全匹配；或引导键导航候选的既定置顶） | 码表引擎（`code==input`）；协调器精确码短语（`lookup`） |
| `is_sentence` | bool | 引擎合成的整句解（Viterbi 多词/超长整词） | 拼音引擎 |
| `is_sentence_demoted` | bool | 整句已让位（① 精确整词 ② 用完残码的补全；引擎侧把 weight 压到 `max-1`） | 拼音引擎 |
| `is_phrase` | bool | 全局短语（`self.phrases` 注入的，系统/用户皆然） | 协调器短语注入 |
| `is_command`/`is_group` | bool | `$CC` 命令 / `$SS·$AA` 组——**决定选中行为，不参与排序** | 短语注入 / `finalize_candidates` |
| `weight` | i32 | 权重（**只承载真实词频**；跨来源仍不可比，见 §3 红线①） | 引擎 |
| `base_order` | i32 | 词库**层级基序档**（`[[dictionaries]].base_order`，默认 0，小整数即分档） | 码表词库配置 |
| `natural_order` | i32 | **每库局部**出现序（各库从 0 数，仅同 `base_order` 档内可比） | 词库解析 |
| `consumed_length` | usize | 该候选消费的输入长度（分段上屏用；0=未标注/整串） | 拼音引擎 |
| `source` | enum | None/CodeTable/Pinyin/English/Phrase | 引擎 |

---

## 3. 三条贯穿全文的红线

**① 权重量纲不可比——跨类别比 `weight`，比的其实是类别。**
词组权重取自词频、单字取自字频，两套量纲不可比（`codetable/engine.rs` 自认）。更有甚者，多套系统往
`weight` 上叠**巨大常量**来表达「类别」：拼音 `BARE_INITIAL_SINGLE_CHAR_BOOST=10M`、
（已删的）协调器 `PHRASE_WEIGHT_BASE=40M`。这些数字**没有物理意义**，只是排序占位符。
**任何「跨来源比权重」的想法都要先想到这一点。**

> 三处最重的欠账都已还清：混输引擎的六个加成常量整体拆除、改为显式的**截断优先级档**（§4.3，
> 记录见 `docs/design/mixed-source-tier-quota.md`）；词库权重有了方案级归一化
> （`docs/design/dict-weight-normalization.md`）；`PHRASE_WEIGHT_BASE` 随之删除，短语改按
> 自身权重与码表精确候选竞争（§5）。
>
> ⚠️ **红线①并未因此作废**，它换了形态：现在「同轴」是一条**数据契约**（自产权重一律落在
> `0~WEIGHT_RANGE_MAX`=10000：短语默认 1000、五笔主库 median 941、`LEARN_ADD_WEIGHT` 800、
> `PROMOTED_WEIGHT` 1000），而契约靠数据守、不靠类型系统。Rime 生态导入的方案（虎码
> p99=343,880）不配 `[weight_spec]` 就会破坏它，且**失效时毫无报错**。护栏有两道：
> `SystemDictLayer::effective_weight` 的查询期越界告警，以及守门测试
> `tests/builtin_phrase_reachability.rs`。

**② 匹配层/精确档先于权重——胜负常在比权重前就被结构标志定死。**
`candidate_display_order` 与 `freq_tier` 都把「层级/档位」作为**首要键**，`weight` 只在同层同档内才起作用。
所以「靠调权重把某候选提上去」经常无效——它可能在更低的匹配层，权重再高也上不来。

**③ 三套排序系统必须口径一致，改一处要核对另两处。**
`candidate_display_order`（用 `is_prefix`/`is_exact_code`）、`freq_tier`（用 `is_phrase`/`source`/`code==input`）、
混输引擎加成（用 `is_phrase`/`code==input`）是**平行实现**，各自有一套「谁该靠前」的判据。历史上多次
「只改一套、另一套绕过」导致行为漂移。**当前的对齐口径：完全匹配（精确码）才提前，前缀匹配一律避让。**

---

## 4. 引擎内排序（第 1 步）

各引擎产出候选时**先自排一遍**。注意：协调器第 5 步会**无条件重排全部候选**，所以引擎内排序的
**匹配层/精确档必须落到 `Candidate` 字段上随候选流动**，否则会被下游按纯权重推翻（「引擎排对了、协调器
又推翻」的白工）。

### 4.1 码表引擎（`codetable/engine.rs`）

- 候选来源：`dm.search`（精确）+ `dm.search_prefix`（前缀补全，精确模式跳过）。
- **只置位 `is_exact_code = (code == input)`**，**从不置 `is_prefix`/`is_partial`**（码表的「精确 vs 补全」
  分层全靠 `is_exact_code` 承载）。
- 排序：`cmp_exact_first(a,b).then(base_cmp)`，`base_cmp` 由 `base_sort` 选：
  - `base_sort = "weight"`（默认）→ `better`：`weight 降 → base_order 升 → natural_order 升 → code → consumed_length 降 → text`
  - `base_sort = "natural"` → `by_natural`：**完全忽略权重**，`base_order 升 → natural_order 升 → code → consumed_length 降 → text`
- **空码补全**：精确模式无候选且未满码时，从更长编码取首个作 `completion_hint`（**只备货不入列**，交协调器判空）。

### 4.2 拼音引擎（`pinyin/mod.rs`）

- 置位 `is_fuzzy`/`is_prefix`/`is_partial`/`is_sentence`/`is_sentence_demoted`；**约定不置 `is_exact_code`**
  （混输下码表精确恒先于拼音，靠这个约定）。
- 排序首要键 `cmp_match_layers`（见 §6），再按权重等。
- 特例：**裸声母**（无完整音节，如 `m`/`zh`）单字 +`BARE_INITIAL_SINGLE_CHAR_BOOST=10M` 提到多字词前；
  **残码前缀补全**标 `is_promoted_completion=true` 上浮进完整匹配层（`is_prefix` 保持结构真值，
  否则数百单字淹掉目标词，见 `meiy`→没有 案）。
- **补全折扣**（`COMPLETION_WEIGHT_DISCOUNT=0.5`）：上浮层内部按「未输入的音节数」**连续**打折，
  `w_eff = w × 0.5^extra`。这一级不可省 —— 只有上浮层级而无折扣时，层内**只比裸词频**，
  extra=1 与 extra=3 同等对待：实测 `nih` 下「你会发现」(w=13330, extra=3) 压过
  「你好」(w=5328, extra=1)，且 114 条补全把单字「你」(w=492791) 整层压到第 114 位。
  取 0.5 对齐 librime `kCompletionPenalty`=log(0.5)（`algo/syllabifier.cc:22`，与词频同轴相加
  于 `dict/dictionary.cc:155`）与 fcitx5/libime `overLengthCost = log10(0.5) × lengthDiff`
  （`pinyin/pinyindictionary.cpp:471`）。⚠️ 降级门槛 `COMPLETION_FAR_WEIGHT_FLOOR` 仍比**原始**
  weight（它按原始权重分布标定），折扣与降级是正交的两件事。
- **残码补全整句解码**（step 2c，`lattice.rs::add_partial_final_nodes`）：把尾部残码当作一个
  **待定音节**放进词图，候选 = 以残码为前缀且音节数为 1 的单字，由 Viterbi 选最优
  （`buzhidaok` → 「不知道」+ `k`→「看」→ **不知道看**）。对齐 librime `enable_completion`
  （`algo/syllabifier.cc`）与 fcitx5 的「不完整拼音」。
  惩罚 `PARTIAL_FINAL_PENALTY = ln2`（= librime `kCompletionPenalty` 的量级）。
  ★ **残码位不给单字虚词优待**（`score_node_partial_final`）：`score_node` 给单字虚词
  `FUNCTION_WORD_BONUS`(+2.0)、给实词 `SINGLE_CHAR_PENALTY`(−3.0)，再豁免 `WORD_PENALTY`(3.0)，
  合计 **8.0** 的量级差，碾压任何词频差距 —— 实测补出「中华**让**」而非「中华人」、
  「你好**们**」而非「你好吗」（让/们在虚词表，人/吗不在）。该优待的前提是「虚词随内容词
  出现是语法黏着」，描述的是**整句内部已成形的搭配**；残码位是「用户打到一半的那个音节」，
  是虚词的先验并不比实词高，前提不成立。**同一条加成在两个位置前提不同 ⇒ 按位置区分，
  不按词性区分。**
  ★ **不能让 `add_abbrev_nodes` 兼职**：二者都是「补音节图给不出的节点」、代码形状几乎一样，
  但简拼节点会把**已完成的音节也重切**成声母序列。实测放开简拼闸门让残码入图，
  `buzhidaok` 产出「不直达欧卡」、`nihaom` 产出「你黑暗欧美」——`bu zhi dao` 被拆回
  b/u/zh/i/d/a/o 去凑简拼了。残码补全是简拼组句的**严格子集约束**，必须独立成路。
  ★ **必须另起一条路径，不能改 step 2**：step 2 建图在 `completed` 上，`nodes` 长度只到
  `completed.len()+1`，**残码末端没有槽位**，Viterbi 到不了串尾（这正是原注释「lattice 到不了
  残码末端、整句退化成单字」的约束）。step 2c 在含残码的 `query` 上重建图。
  ★ 两条路径的产出**都保留**（不同 `consumed_length` 层）：`nihaom` 既给整句「你好」
  (consumed=5，选它则 `m` 留缓冲续输)、也给残码整句 (consumed=6)。**改成「用残码整句替换
  step 2 结果」会破坏分步上屏。**
  ★ 门槛 `syllables.len() >= 2`（同 step 2）：`nim` 这类 1 音节 + 残码不走，那种输入的正解是
  词库补全（你们/你没），残码整句「你吗」只会挤掉它。同 fcitx5 `partialLongWordLimit` 的精神。
  双拼跳过（`query` 是转换后全拼、与击键不同域），分隔符跳过（`completed` 由音节 join 得出，
  与 `query` 字节位不同源）。
- **整句让位于「用完残码的补全」**（step 6.5b）：残码存在时，整句只解释 `completed`、**把用户
  已按下的残码丢掉**，却靠 `SENTENCE_WEIGHT_BASE`(3e7) 无条件置顶 —— 实测 2/3/4/6 音节一律
  如此（`nihaom`→你好 而非 你好吗、`zhongguor`→中国 而非 中国人）。判据复刻 librime
  `has_exact_match_phrase`（`gear/script_translator.cc:387`：存在覆盖完整输入的精确词条时
  **不生成整句**）：**补全词音节数 == completed + 1**（残码补成一个音节后恰好用完）。
  手法同 6.5 —— 降到该批补全的 `max-1` 并标 `is_sentence_demoted`，**降级不销毁**。
  ★ 判据自带过滤：`beijingdaxuex` 的「北京大学校长」6 ≠ 4+1 不触发，w=4 的冷僻预测词
  因此顶不掉「北京大学」；换成「extra ≤ 2」一刀切就会放它进来。
  ★ 之所以降 weight 就能换位：残码补全经上浮 `is_promoted_completion=true` ⇒ `eff_prefix`
  为 false，与整句**同层**；若不同层则跨层不比权重，降多少都没用。
  ★ **让位须过置信度门槛** `SENTENCE_YIELD_WEIGHT_FLOOR`：否则 `zhonghuar` 的「种花人」
  (w=0，音节数恰好 3=2+1) 就能顶掉整句「中华」并把它压成 **w=-1**。librime 不需要这道门槛
  （整句与词条同轴，w=0 自然排不上去）；我们的整句跨轴置顶、让位只能是二值开关，
  **把连续比较压成布尔，就得自己补回被丢掉的门槛**。
- **上浮距离收到 1**（`COMPLETION_UNCONDITIONAL_FLOAT_SYLLABLES`）：只有「补完手头这个音节」
  （距离 1）无条件上浮，距离 ≥2 一律要过 `COMPLETION_FAR_WEIGHT_FLOOR`。旧值 2 让距离 2
  整档白白豁免门槛，w=18 的「中华人民」因此在 `zhonghuar` 时登顶、压过整句「中华」——
  这正是用户报的「候选长度来回跳动」：有残码时冷僻长词靠豁免登顶、无残码时整句 3e7 登顶，
  两套依据逐键切换。⚠️ 该常量**不是** `COMPLETION_NEAR_SYLLABLES`（后者只管用户词长词上浮，
  两者一度共用一个 2，改动时连带打断 `qingfengshu`→「清风输入法」）。

### 4.3 混输引擎（`mixed/engine.rs`）——**截断优先级档**

混输把码表半边 + 拼音半边合并，按 `MixedEngine::truncation_tier` **稳定排序**，
再去重、截断：

| 档 | 对象 |
|---|---|
| 0 | 码表精确全码（`code == 判据串`） |
| 1 | 短语（**本引擎恒不可达**：`is_phrase` 无生产置位点，短语由协调器在引擎之后合并） |
| 2 | 码表前缀补全/拆分、英文整词 |
| 3 | 拼音全部、英文前缀 |

**本档位只回答「谁活下来」，不回答「谁排前面」**——最终显示序由协调器
`candidate_display_order` 无条件重排全部候选决定（§6）。

> ⚠️ **排序键只有「档」一个，档内绝不可再排**。`sort_by_key` 是稳定排序，而候选按
> `码表 → 拼音 → 英文` 拼接、每段内部已是子引擎排好的序 ⇒ 同档保持传入次序即**子引擎原序**。
> 档内若按 weight 重排，拼音的 `cmp_match_layers` 就会被抹掉（层级键是布尔的、等价于惩罚 ∞，
> weight 表达不了），高词频简拼会反超低词频精确候选。

> **历史**：这套档位从前编码在 weight 的**数值大小**里——短语 +1M、码表精确
> +`codetable_weight_boost`(1e7)、码表前缀补全与英文整词各 +500K、拼音 ÷100，然后全局按 weight
> 排序。代价是真实词频与类别偏置挤在同一个 `i32`（拼音 p50=34 被整除归零），且偏置随候选一路
> 泄漏进协调器的显示序。拆除过程见 `docs/design/mixed-source-tier-quota.md`。

**截断的拼音保底配额**（`truncate_with_pinyin_quota`，`convert` 与 overflow 共用）：码表在档 0/2、
拼音在档 3 ⇒ 截断时码表恒在前，而五笔 2 码前缀的候选量常常吃满整个配额——实测 **52 个 2 码前缀的
条目数 > 300**（最多 `kh` 663 条），其中 `pu`（495 条）正是「既是五笔 2 码、又是完整拼音音节」的
交集。那种输入下拼音候选**一条都进不了列表**，协调器 §6 ③ 的拼音精确档就无从下手（提不了不在场的
候选）。故截断时给拼音留 `max/PINYIN_QUOTA_DIVISOR` 席。

⚠️ **英文没有配额**：档 2/档 3 里的英文候选被码表洪水挤掉时无保底。见
`mixed-source-tier-quota.md` §3.3。

- **只补不挤空**：尾部确实没有拼音候选时（`kh` 这类非音节码、纯五笔溢出串）一条码表候选都不会被
  挤掉，行为与改动前完全一致（回归测试 `no_pinyin_means_no_codetable_is_evicted`）。
- ⚠️ 补进来的拼音候选**追加在尾部、不保证有序**——这依赖协调器第 5 步会无条件重排全部候选。
  本函数的职责只是「让候选进得来」，不是「排好序」。

---

## 5. 全局短语的注入（第 3 步，`handle_candidate.rs`）

全局短语（`self.phrases`，系统/用户皆然）**不与方案挂钩、是跨方案的**，故需特殊处理。**按「来源=全局短语」
统一处理，不按 `$CC`/`$SS`/静态语法类型区分**（语法只决定 `is_command`/`is_group` 的选中行为）：

| 匹配方式 | 来源 | `is_exact_code` | `is_prefix` | `weight` |
|---|---|---|---|---|
| **精确码**（`lookup`，`code==输入`，HashMap 精确键） | 完全匹配 | **true** | false | `hit.weight` |
| **前缀枚举**（`lookup_prefix`，码严格更长） | 前缀匹配 | false | **`!codetable_mode`** | `hit.weight` |

两条通路的 `weight` 现已同口径。精确码短语此前是 `PHRASE_WEIGHT_BASE(40M) + hit.weight`，
该常量已删除——短语按自身权重与码表精确候选竞争，「谁排前面」交回给权重配置。

- **精确码短语**（打全 `date`）→ 进精确档，与码表精确候选**同层比权重**（对应「完全匹配才提前」；
  `is_exact_code` 漏标会让它掉到精确档之下，那是 `skce` 短语曾输给五笔「可能」那个 bug 的修复点）。
- **前缀短语**（打 `da`）→ 不进精确档；`is_prefix=!codetable_mode` 使其在拼音/混输降到拼音精确候选之下、
  码表下与更长编码补全同档（对应「前缀避让、按权重」）。

### 5.1 ⚠️「精确码短语 vs 码表精确」的裁决者，按模式与开关而异

沿本文 §6 的比较链倒推各模式的**实际**裁决者。删 40M 与档位合并是**先后两步**，各自的影响面
不重叠——分开做才分得清因果：

| 模式 / 开关 | 裁决者 | 删 40M | 档位合并 |
|---|---|---|---|
| **纯码表**，调频关 | 两者 `is_exact_code` 同为 true ⇒ `cmp_exact_first` 平局 ⇒ 落到 `weight` | ✅ **变**：真比权重 | ⭕ 不经过 `source_tier` |
| **纯码表**，调频开 | `freq_tier`（=`source_tier`）是首要键，整体压过显示序 | ⭕ 档位隔离，码表恒赢 | ✅ **变**：不再推翻权重序 |
| **混输** | `source_tier` 在 `weight` 之前 | ⭕ 40M 早被档位覆盖 | ✅ **变**：同档比权重 |
| 纯拼音 | 拼音候选 `is_exact_code` 恒 false ⇒ `cmp_exact_first` 已让短语居前 | ⭕ 与权重无关 | ⭕ 不经过 `source_tier` |

**一个「恒赢的偏置」往往只在你没检查的那一条路径上恒赢。** 40M 存在时纯码表下的那次比较从未真正
执行过，删除后才第一次生效——与混输六个加成拆除时暴露的是同一类账。

而删掉 40M 又**留下了一处开关依赖的不一致**（上表前两行：同为纯码表，开/关调频结论相反），
这正是随后合并 tier 0/1 要偿还的账。详见 §7.3「tier 0 为什么把短语并进来」。

**内置数据的隐式契约**：系统短语权重必须高于同码码表词条，否则打那个码时首选不是短语。
全仓 54 个系统短语码里 `datm`（对手五笔「万花筒」w=1080）和 `tmts`（「身条」w=536）与五笔碰撞，
前者曾因短语权重 1000 更低而失效。守门测试 `tests/builtin_phrase_reachability.rs` 固化了这次扫描
——词库按词频重排后权重会整体变动，靠人是守不住的。
- **方案内词库 `$CC` 词条**（挂在五笔等方案里，走 `finalize_candidates`）**不是**全局短语：它 `is_phrase=false`、
  `source=CodeTable`，按方案权重排、`is_command` 只影响选中行为——**天然按方案处理，不经本节**。

---

## 6. 协调器 `candidate_display_order`（第 5 步，权威显示序）

对**全部候选无条件重排**。`candidate_display_order` 本体是**七级**比较链（⓪~⑥）（`handle_candidate.rs`）：

```
⓪ by_consumed             **消费输入长度降序**（P0，首要键）
                          对齐 librime：`DictEntryCollector = map<size_t, DictEntryIterator>` 以
                          「消费的输入长度」为 key、`phrase_->rbegin()` 从最长遍历 ⇒ 消费更多者
                          恒优先，**先于词频、先于任何层级**。`buzhidaok` 的「不知道看什么」靠它
                          从第 136 位到首位（残码 k 终于被响应）。
                          ⚠️ `consumed_length == 0` = 「未标注按整串算」（**码表候选恒为 0**），
                             必须归一化成 input_len 再比，否则码表候选整体被甩到最后。
                          ⚠️ **引擎侧刻意不用这个键**：那边 `truncate` 紧随排序，用它会让消费更少的
                             候选（`xi'an` 的「西」、`nihao` 的「你」）被整批丢弃而非仅仅排后
                             （实测红 10 条，含两条真回归）。根因是架构差异——librime 的
                             `Translation` 惰性流式从不全局截断，我们是一次性产生 N 条 + 截断。
① cmp_match_layers        is_abbrev 升 → **eff_prefix** 升 → is_partial 升
                          （`eff_prefix = is_prefix && !is_promoted_completion`，与引擎、
                            `freq_rerank` 共用同一个函数，三处不得各写一份）
                          ⚠️ 曾在协调器另写过一份「同构但忽略 `is_promoted_completion`」的副本，
                             动机是该标志本为「让高价值补全活过引擎 truncate」而设、协调器不截断。
                             **动机成立但代价被漏算**：层级键是布尔的 = 惩罚 ∞，于是引擎侧一切
                             **用 weight 表达的让位**在协调器全部失效 —— step 6.5b 把整句压到
                             「补全 weight - 1」让位给恰好用完残码的补全，到协调器却因补全停在
                             `is_prefix` 层而被整句反超（`nihaom` 首选「你好吗」→「你好们」、
                             `beijingd` →「背景的」）。已还原为直接调用 `cmp_match_layers`。
                             当初的动机也已消失：`zhonghuar` 的「种花人」(w=0) 能登顶是因为当时
                             无候选消费到第 9 字节，step 2c 残码整句落地后它自然被压下去。
                          ⚠️ is_fuzzy **已不是层级键**（曾是首要键 = 惩罚 ∞，把「是」压到第 231 位），
                             模糊音改走 weight 折扣 FUZZY_WEIGHT_SCALE=0.01；补全同理走 0.5^extra
② cmp_exact_first         is_exact_code 降                          （同层内精确档优先）
③ cmp_pinyin_exact_first  拼音精确档 降   （**仅混输**；is_pinyin_exact_tier，先于码表前缀补全）
④ by_weight               weight 降       （base_sort=natural 时 ignore_weight 跳过本级）
⑤ base_order              升              （词库档位，跨库隔离）
⑥ natural_order           升              （每库局部出现序）
   （原末级 `consumed_length 降` 已上升为首要键 ⓪ —— 它此前排在第 7 级，前六级早就分出
     胜负，等于从不生效；`buzhidaok` 的残码被忽略正是这么来的）
```

**③ 拼音精确档（混输专属）**：判据 `wind_candidate::is_pinyin_exact_tier(c, input_len)` =
`source==Pinyin && is_common && !is_prefix && !is_partial && !is_abbrev && !is_fuzzy`
**且消费整串**（`consumed_length == 0 || consumed_length >= input_len`，0=未标注按整串算）。
于是三档顺序为「码表精确/精确码短语 → **拼音精确** → 码表前缀补全」。

- ⚠️⚠️ **「消费整串」必须直接问 `consumed_length`，不能拿 `!is_partial` 代替**（首版即栽于此）：
  真机打 `aaw`（本意 `aawt`→工作）首选变成拼音「啊啊」。它是 Viterbi 整句（词条 `啊啊 a a`），
  `code` 取 `completed`="aa"、`consumed_length=2`，只解释了 3 键中的 2 键——**可 `is_partial`
  是 false**：整句走 `insert(0)` 不经 `pinyin/mod.rs` 里算 `is_partial` 的 `push_hit` 闭包，
  同文合并时还会主动 `existing.is_partial = false`。
  ★ `is_partial` 的语义是「这不是子短语」，**不是**「消费了整串」，两者在残码场景下分叉。
  ★ 该场景比 `xu` 更严苛：五笔 `aaw` **无精确全码**（候选全是前缀补全），没有
  `is_exact_code=true` 的候选占着首位 ⇒ 拼音一旦被误提档就直接是首选。
  回归测试 `mixed_aaw_partial_sentence_does_not_preempt_codetable`（断言首选是「工作」）。

- **修的什么**：混输打 `xu`，码表精确「弱」是首选（`xu` 是二简码），但拼音「需」（`code==xu`、
  该音节最高频字 6999）**实测排第 98 位**——被 124 条 `xu*` 码表前缀补全整体压住
  （`per_page=5` ⇒ 第 20 页，与真机报告精确吻合）。根因是「精确 vs 前缀」这个维度混输此前
  只承认码表那一半：码表精确 +1e7、码表前缀补全 +500K，拼音**不分精确与补全**统一 ÷100。
  拼音侧的 `is_prefix`/`is_partial` 信息一直齐全，只是在 `normalize_pinyin` 被抹平。
  （那批加成现已拆除，见 §4.3；本条记录的是问题当初的成因。）
- **为何是层级键而非权重加成**：拼音词库最高权重是「的」**15,378,475**，任何「不先归一就加
  boost」的写法都会让它越过码表精确档 1e7（打 `de` 首选变「的」）。层级键无量纲陷阱（红线②）。
- **为何叠 `is_common`**：单音节同音字动辄上百条（`xu` 有 **329** 条，含权重 0 的生僻字）。
  用检索范围而非固定条数上限把关，让「提多少条」跟着用户的 `filter_mode` 走。
  ⚠️ 依赖第 4.5 步 `mark_common` 无条件跑过——该判定原先写在 `apply_filter` 内部、
  `Gb18030` 时随 early-return 一起被跳过，沿用那个位置会让本档在 Gb18030 下**静默失效**。
- ⚠️ **必须按引擎语境传 `mixed`，不可恒 true**：纯拼音下全体候选同为 `Pinyin` 来源，本键会退化成
  「`is_common` 优先」，把含生僻字的多字词（`is_string_common` 要求整串每字都常用）硬降到全部
  常用单字之后。回归测试 `pure_pinyin_xu_order_is_unaffected`。
- ★ 这是「五笔优先」的一处**有意松动**：码表**精确**仍恒先于拼音（①②不变），只有码表**前缀
  补全**让位。依据是短输入下二者置信度反相关——124 条补全全都要打满 4 码才精确，而拼音 `xu`
  已是完整音节。

**关键点：**
- ①②③是**结构层级**，④才是权重——所以「靠权重反超」只能在同层同档内发生（红线②）。
- `cmp_exact_first` 置于 `cmp_match_layers` **之后**：精确优先只在同匹配层内生效，不跨层提拔
  （`is_prefix=true` 的前缀短语仍留在下层）。
- **⚠️ 本函数末级是 ⑥ `consumed_length`，没有 `text` 末级。** 主排序（第 5 步）对①~⑥全同分的候选
  靠 `sort_by` 的**稳定性**维持引擎/注入序。**凡需要「确定性取一条」的调用方（如空码补全池取首条）
  必须自己补 `.then_with(|| a.text.cmp(&b.text))`**——`lookup_prefix`/HashMap 遍历序不定，不补末级会
  退化成「随机取一条」、重启可能不同（补全池已补，见第 4 步）。

---

## 7. 词频重排 `freq_rerank`（第 8 步）——**开自动调频时的真正话事人**

### 7.1 触发前提（`apply_freq_rerank`）

**全部满足**才跑，否则直接跳过（此时 §6 的显示序即最终序）：
1. 有 `store`；2. `code` 非空且候选 ≥ 2；3. **自动调频开启**（`freq_settings().enabled`）；
4. **至少一个候选有词频记录**（`recs` 非空，`count>0`）——**全新无记录时不跑**。

### 7.2 引擎分派

- `is_pinyin()`（纯拼音）→ `rerank_pinyin_decay`（§4 衰减软置前）
- 其余（**码表 / 混输**）→ `rerank_codetable_usedfirst`（§3 永久 used-first）

### 7.3 码表/混输：`rerank_codetable_usedfirst` + `freq_tier`（**首要键**）

`freq_tier`（越小越靠前）：

| tier | 对象 | 判据 |
|---|---|---|
| **0** | 码表精确全码 **+ 精确码短语** | `source==CodeTable && code==input`；或 `is_phrase && is_exact_code` |
| **1** | **拼音精确档** | `is_pinyin_exact_tier(c)`（与 §6 ③ **共用同一判据函数**） |
| **2** | 码表前缀补全 | `!is_phrase && source==CodeTable && code!=input` |
| **3** | **前缀短语** + 其余来源 | `is_phrase && !is_exact_code`；或 `_ =>`（主要是 `CandidateSource::None`） |
| **4** | 拼音其余（前缀/子短语/简拼/模糊/生僻） / 英文 | `source==Pinyin/English` |

> tier 1 **不必**像 §6 那样区分「是否混输」：`freq_tier` 只服务 `rerank_codetable_usedfirst`
> （码表/混输），纯拼音走 `rerank_pinyin_decay` 不经过此处；纯码表下没有 `Pinyin` 候选，该档天然
> 是空操作。**但两处的判据函数必须是同一个**——只改 §6 一侧，开自动调频时会被 `freq_tier`
> 整体绕过（红线③）。

#### ⚠️ tier 0 为什么把短语并进来

精确码短语曾独占 tier 1（码表精确恒在其前）。合并的动因**就在本节**：`freq_tier` 是这里的
首要键、整体压过 §6 的显示序，而 §6 在 `PHRASE_WEIGHT_BASE`(40M) 删除后已按权重比二者。
二者分档就会出现**开关依赖的不一致**——同一个输入，开自动调频码表精确恒赢、关调频按权重比，
而 `[schema.codetable.frequency] enabled` 默认关，用户打开它才撞上，且看不出关联。

调频重排的比较链**不含 weight**（§8），同档无记录者返回 `Equal` ⇒ 稳定排序维持 §6 喂进来的
权重序。所以合并不是「让本节去比权重」，而是**让本节不再推翻权重序**。

回归测试 `freq_rerank::tests::codetable_tier_exact_phrase_keeps_coordinator_weight_order`
双向断言；把档位改回分档可让其中第 ① 条变红（已实测），第 ② 条方向与旧行为一致、单独测会假绿。

**前缀那一对刻意不合**（前缀短语 tier 3、码表前缀补全 tier 2）：归一化让量纲可比了，但
**可比 ≠ 该比**。档位还表达置信度——精确码短语是「打全了码、明确要它」，前缀短语是「只打了
前缀、系统猜他要」。且前缀短语权重（新建默认 1800）普遍高于码表前缀补全（五笔 median 941），
合档会复现用户报过的「短语前缀匹配时优先级偏高、压普通编码」。

- **档内**再按 used-first：`Step`（count 降、last_used 破平，抗误选）/ `Top`（last_used 降、count 破平，MRU）；
  同档无记录者返回 `Equal` → **稳定排序维持第 5 步喂进来的显示序**。
- **首选保护 `ProtectPolicy`**：重排后把「基础序前 N 位」回填锁定（呈现层保护，不动 weight）。
  N **按输入码长分级**（`schema.codetable.frequency.protect_top_n_len{1,2,3}` + `protect_top_n` 兜底）：

  | 输入码长 | 出厂 N | 理由 |
  |---|---|---|
  | 1（一简位） | 1 | 五笔一简 25 个码**每个都是二选一**（`a` → 工 9999 / 戈 9998） |
  | 2（二简位） | 1 | 同上，616 个码中 39 个有竞争者 |
  | 3（三简位） | 0 | 钦定性弱于一二简 |
  | ≥ 4（全码位） | 0 | 调频该起作用的地方 |

  ⚠️ **这是简码钦定次序的唯一防线**：词库靠权重表达简码地位（`gen_dict` 一简 9999 / 二简 9950 /
  三简 9000，普通词条压在 9000 以下），而**本节的比较链不含 weight**——权重再高也拦不住
  「被选过」这一位。设计与数据统计见 `docs/design/codetable-freq-short-code-protection.md`。

  保护名额**只在精确档内取**（`is_exact_code`）：钦定首选必在精确档，名额匀给前缀补全没有语义
  依据；精确候选不足则少保护，该码位无精确候选则不保护。
- ⚠️ 保护作用域是「**码表配置组**」而非「纯码表方案」：`freq_settings()` 按"非拼音即码表"分流，
  **混输走的是同一套值**（混输下 2 键拼音会落进"二简位"档——有意保留，此时基础序首位本就是
  码表精确候选，保护它与"五笔优先"同向）。拼音路径恒为 `ProtectPolicy::NONE`。
- ⚠️ **`freq_tier` 是首要键，开自动调频时整体压过 `candidate_display_order`**，也因此**掩盖
  `is_exact_code`/`is_prefix` 的效果**——验证 §6 类改动**必须关自动调频**。
- ⚠️ tier 1 vs tier 3 对短语的区分**依赖 §5 打好的 `is_exact_code`**：精确码短语 tier 1、前缀短语 tier 3
  与码表补全同档（这是「打 `da` 时 `date` 短语不再压过码表补全」的落点）。

### 7.4 纯拼音：`rerank_pinyin_decay`

- **锚定**：只有 `is_phrase && is_exact_code`（精确码短语）恒锚定顶部、互相维持引擎权重序。
  前缀短语（`is_phrase && !is_exact_code`）不锚定，落到下面 `cmp_match_layers` 靠 `is_prefix`
  降到精确候选之下（与 §7.3 `freq_tier` tier1/tier2、§6 `candidate_display_order` **同口径**：
  完全匹配才提前、前缀避让）。
- ★ **整句不锚定**：整句 weight 已与词库同量纲（`pinyin::sentence_weight`），靠
  `candidate_display_order` 挣位置，然后与其余候选一样接受词频挑战。此前 `is_sentence` 也在
  锚定之列，那是硬闸门——命中即维持原序、衰减分连算都不算，于是「整句同量纲」只在无词频
  记录时成立。移除的实测影响面极小，锚定早被两侧夹到名存实亡：同码同层的竞争者会让引擎侧
  step 6.6 置 `is_sentence_contested` 摘掉锚定（该步骤与字段此后一并删除），不同层的候选
  则被 `cmp_match_layers` 挡在 target_pos 之前推不动。唯一真变化是**模糊同码候选**（6.6 的
  过滤器带 `!o.is_fuzzy`，不算竞争者）现在能靠词频反超整句。守门测试
  `pinyin_fuzzy_peer_can_overtake_sentence`
  + `pinyin_sentence_is_not_pulled_back_to_top`。
- 其余：先 `cmp_match_layers`（不跨层提拔），再按衰减分（半衰期）软置前，褪色（< ε）落回权重序。

> ✅ **已对齐**：此前 `|| is_phrase` 一刀切锚定所有短语，导致纯拼音下 `date` 前缀短语在「自动调频开
> 且有词频记录」时被顶到首位（潜伏 bug，实测复现）。已改为 `|| (is_phrase && is_exact_code)`，与
> `freq_tier` 口径一致。回归测试 `pinyin_exact_phrase_is_anchored_like_sentence`（精确码短语仍锚定）
> + `pinyin_prefix_phrase_not_anchored`（前缀短语不锚定）。

---

## 8. Shadow（第 9 步，最高优先级）

`apply_shadow`：按用户 shadow 规则先删 `deleted`、再把 `pinned` 词移到指定位置。**优先级最高**——
用户手工置顶/删除的意图压过一切算法序。按 `data_schema_id` 归属（拼音族折叠共享、码表/混输各自独立）。

> ⚠️ 「最高优先级」**不再由「排在最后」兑现**。出简让全（第 9.5 步）排在它之后——那是
> `apply_freq_rerank` 强加的次序（深码位 `ProtectPolicy.fallback = 0` 不设保护，先让位会被调频原样
> 顶回去），而 shadow 恰好夹在两者之间，没有能同时满足两个约束的位置。于是优先级改写成**判据**：
> `apply_shadow` 返回本码是否有 `pinned` 规则就位，让位据此对整码停手。
>
> 判据只在 `apply_shadow` 一处取值，让位侧**不得**自己再查一次规则——`(schema, code)` 的取法有
> `data_schema_id` 折叠与 `shadow_code_of` 归一两道，两处各取一次迟早失配且完全静默。
>
> 只数 `pinned` 不数 `deleted`：删除说的是「这条别出现」，与「谁排第一」不是同一维度。
> 详见 `docs/design/codetable-short-code-yields-full.md` §6.2。
>
> **给后来者**：任何新的重排若排在 shadow 之后，都要回答同一个问题——它凭什么可以推翻用户手工
> 排定的顺序。默认答案是不能，做法照抄第 9.5 步的判据。

## 9. `apply_filter` 与 `expand_s2t_variants`

- **`apply_filter`（第 7 步）**：按检索范围（常用字表 / GB18030）过滤。放行判据是
  `is_common_like(c) || rare_phrase_admits(c, phrase)`：前者＝ `is_common` 或用户自己配的
  短语/命令/分组（故 **`is_phrase` 候选恒保留**）；后者＝ `input.rare_phrase = "keep"`（出厂）
  下的**多字**候选，「多字」按字素簇算，与 `is_phrase` 无关（单个 emoji 不是词）。
- **`expand_s2t_variants`（第 11 步）**：简繁 1对多变体紧跟单字原字插入。**硬约束**：必须在
  去重/排序/词频/shadow **全部完成后**（否则去重按 text 误删变体、重排拆散原字与变体）、且在
  **自动上屏判定之后**（否则变体会让「唯一候选」误判为不唯一，静默否决自动上屏）。

---

## 10. 常量速查

| 常量 | 值 | 位置 | 作用 |
|---|---|---|---|
| ~~`PHRASE_WEIGHT_BASE`~~ | ~~40,000,000~~ | ~~`coordinator.rs`~~ | **已删除**。曾是精确码短语的权重基，现按 `hit.weight` 与码表精确候选竞争（§5.1） |
| `PINYIN_QUOTA_DIVISOR` | 5 | `mixed/engine.rs` | 截断时拼音**保底配额**分母（`max/5`，300 ⇒ 60 席） |
| `BARE_INITIAL_SINGLE_CHAR_BOOST` | 10,000,000 | `pinyin/mod.rs` | 裸声母单字提权 |
| `COMPLETION_WEIGHT_DISCOUNT` | 0.5 | `pinyin/mod.rs` | 前缀补全**每个未输入音节**的权重折扣（`w × 0.5^extra`） |
| `COMPLETION_UNCONDITIONAL_FLOAT_SYLLABLES` | 1 | `pinyin/mod.rs` | 补全无条件上浮的最大距离；≥2 须过下面那道门槛 |
| `COMPLETION_FAR_WEIGHT_FLOOR` | 100 | `pinyin/mod.rs` | 远距离补全上浮的词频门槛（挡住 w=18 的「中华人民」，放行 w=2010 的「北京大学」） |
| `SENTENCE_YIELD_WEIGHT_FLOOR` | 50 | `pinyin/mod.rs` | 6.5b 整句让位的置信度下限（折后值，等价原始 100） |
| `COMPLETION_NEAR_SYLLABLES` | 2 | `pinyin/mod.rs` | ⚠️ **只管用户词长词上浮**，与上面三个无关 |
| `PARTIAL_FINAL_PENALTY` | ln2 ≈ 0.693 | `pinyin/lattice.rs` | step 2c 残码待定音节的 log_prob 罚（对齐 librime `kCompletionPenalty`） |
| `PARTIAL_FINAL_NODE_LIMIT` | 12 | `pinyin/lattice.rs` | 残码跨度最多取几个单字进词图 |
| `LEARN_ADD_WEIGHT` | 800 | `coordinator.rs` | 加词/学习临时权重 |
| `freq_tier` | 0/1/2/3 | `freq_rerank.rs` | 词频重排档位（见 §7.3） |

---

## 11. 不变量与红线清单（改排序前逐条自检）

1. **完全匹配才提前，前缀匹配一律避让**——精确码短语（`lookup`）可入精确档；前缀短语（`lookup_prefix`）
   一律降层/按权重。三套系统都按此口径。★ 混输的拼音精确档（§6 ③ / §7.3 tier 2）是这条口径的
   **贯彻而非例外**：它让「拼音完全匹配」也享受到此前只有码表才有的提前，代价是码表**前缀补全**
   （非完全匹配）让位。码表精确仍恒先于拼音。
2. **跨来源比 `weight` 无意义**（红线①）——量纲不可比 + 巨大类别常量。别用权重表达「类别」，用显式标志
   （`is_exact_code`/`is_prefix`/`is_phrase`）。
3. **匹配层/档位先于权重**（红线②）——想让某候选靠前，先确认它在对的匹配层/档，而不是加权重。
4. **改一套排序、核对另两套**（红线③）——`candidate_display_order` / `freq_tier` / 混输加成 / 拼音锚定
   是平行实现，漂移无编译报错。
5. **验证匹配层/精确档类改动必须关自动调频**——否则 `freq_tier`/锚定会掩盖效果，误判「没生效」。
6. **`candidate_display_order` 必须在 `freq_rerank` 之前**——后者靠稳定排序继承前者的权重序，自己不比权重。
7. **末级 `text` 兜底不可省**——破 HashMap/遍历序不定。
8. **新增排序标志字段要穷举「谁属于这一层」**——全仓 `Candidate{..}` 都以 `..Default::default()` 收尾，
   漏标默认 false、编译器抓不到。
9. **码表引擎从不置 `is_prefix`**——其精确/补全分层靠 `is_exact_code`；拼音靠 `is_prefix`/`is_partial`。
   两种引擎分层维度不同，写跨引擎逻辑时勿混。

---

## 12. 已知待办 / 存疑

- ~~拼音锚定平行漂移~~ **已修**（§7.4）：`rerank_pinyin_decay` 改为只锚定精确码短语，与 `freq_tier` 对齐。
- ~~混输加成系统 vs 匹配层~~ **已完成**：六个加成常量全部拆除，引擎侧改为显式的截断优先级档
  （§4.3），`weight` 从此只承载真实词频。过程与两处「加成掩盖了未定义行为」的发现见
  `docs/design/mixed-source-tier-quota.md`。
  - ⏳ **仍待做**：英文没有保底配额（同上文档 §3.3）。
- **混输拼音的召回面收窄（未做）**：主流实现（微软、冰凌）在混输下**不给拼音做前缀补全/模糊**，
  只认精确音节。本仓已按此方向定案但**尚未实施**，落点是 `pinyin::Config` 新增 `enable_completion`
  + `schema.mix` 开关，经 `manager.rs::build_engine` 注入（与 `enable_pinyin_abbrev` 同一条路）。
  ⚠️ **实施前必须先改造清空守护**：`mixed/engine.rs` 注释与 `clear_blocked_by_candidates` 明文依赖
  「`wanl` 出前缀补全候选 → 拦住清空」，而这条兜底恰恰只在 `auto_commit_block_on_pinyin=false` 时
  是唯一防线。正确切法是区分「补全不进候选列表」与「引擎内部仍知道有更长码」——后者要保留。
  回归测试见 `input_flow.rs` 末尾的 `wanl` 用例。
- **`base_sort=natural` 与短语**：natural 模式忽略权重，短语靠 `base_order`/`natural_order` 默认 0 浮顶，
  与短语「按权重」的新方向是否自洽，待观察。
