# 码表逆切分输入（切分模式）：设计与实施计划

> 需求来源：论坛 [t11 切分模式](https://forum.windinput.com/topic/11)（flypy，2026-08-28，标签「已计划」），
> 待办登记 `.omc/feedback-todo.md` 的 **C0-5**。
>
> 本文是**设计差分 + 实施计划**，不是现状文档。落地后把「现状」部分并入
> [engine-candidate-pipeline.md](../architecture/engine-candidate-pipeline.md) §3，本文只留决策与理由。

---

## 0. 结论摘要

**要做的是一件事**：码表方案下，打满码长的一串码**一条候选都出不来**时，把它按固定切点切成两段，
各查一次词典，把两段的结果拼成候选——`hfkn` → `hf`(很可) + `kn`(能) → 「很可能」。

**为什么这件事值得做**（flypy 的编码空间论证，本仓需复核，见 §7）：四码音形方案的二简空间
26×26=676，拼音声韵组合只占其中约 400 组，余下的码位放简词。于是「二简字 + 二简词」的组合码
形如「声韵声声」/「声声声韵」，与二字词的「声韵声韵」**结构性错开**，四码空码位天然大量存在，
把它们利用起来等于**用四键打出原本要六键的两个二简**。这是音形方案独有的红利，五笔没有。

**本仓已经有 80% 的基建**：码表整句（`sentence_input`，[codetable-sentence-input.md](./codetable-sentence-input.md)）
已经实现了「把一串码切成多个编码单元并组句」，连同组合区切分显示 `aawt'aawt`、手动分隔符 `'`、
方案级开关、顶码让位。**本功能不复用它的解码器，但复用它的全部出口**，理由见 §2。

**四条已定的取舍**（2026-09-19，与需求方确认）：

| # | 决定 | 备选与放弃理由 |
|---|---|---|
| 1 | 前段固定取首选 × 后段列举 | 帖子正文如此。配图 `hf'kn`→「1.很可能 2.困难」像是前后都出多解，但那会让同前段的变体刷屏（§4.3）。**留旋钮** `split_front_candidates`（默认 1，设 2 即得配图效果） |
| 2 | 只做**逆切分·自动切分** | 手动切分（`编码 + 下滑 空格` 重切）flypy 自评「锦上添花，可以不考虑」；且本仓已有 `manual_separator_key`（`'`）与它语义重叠，两者关系需先想清楚，不在本轮 |
| 3 | 触发判据：**满码且整串零候选** | 最保守：只填「现在什么都没有」的那一格，不抢任何现有候选，回归面接近零。**留旋钮** `split_trigger`，第二档 `no_exact`（无精确匹配即切、候选排末尾、不参与自动上屏）同轮实现，代价 < 30 行 |
| 4 | 切分候选**整串一次上屏**，`consumed_length` 留 0 | 分段上屏会第三次打破全仓「码表候选 `consumed_length` 恒 0」约定（整句刻意没打破，见那篇 §11.5）。本功能的切分产物本就是一个整体，没有分段需求 |

---

## 1. 功能定义：四种切分，本仓已有三种

flypy 给的分类（原帖配图）与本仓现状对照：

| 切分 | 定义 | 本仓现状 |
|---|---|---|
| **顺切分·自动** | 打完四码自动切断与后面编码的关系（`zdup` → 自动上屏） | **已有**：`schema.codetable.auto_commit_at_full`（出厂关） |
| **顺切分·手动** | 用空格或标点打断（`n_u_uw_` → 你是谁） | **已有**：空格上屏 + `schema.codetable.punct_commit`（出厂开） |
| **逆切分·自动** | 打完四码若是**空码**，自动切分为 2+2，即两个二简字/词 | **没有** ← 本文要做的 |
| **逆切分·手动** | 打完编码用切分键按规则重新切分（`ab`→`a\|b`、`abc`→`a\|bc`、`abcd`→`ab\|cd`） | 部分有：`manual_separator_key`（`'`）能显式指定硬边界，但只在 `sentence_input` 开启时放行，且走 Viterbi 而非固定规则。**本轮不做**（取舍 #2） |

> 「切分点前编码对应什么内容就上屏什么内容 = 顺切分；切分点对**前面**编码重新切分 = 逆切分」
> ——原帖的定义。本仓的顶码（`top_code_commit`）属于顺切分族：它是「前 N 码出字、余码续打」。

**候选呈现规则**（原帖「二、实现的一些具体情况」，逐条对应本文判据）：

| 原帖描述 | 本文落点 |
|---|---|
| 前后两个二简都唯一 → 适配四码唯一自动上屏 | §5.1：切分候选 `code == input`，被既有 `decide_auto_commit` 天然认作「恰一个精确匹配」 |
| 前二简有重码、后二简无重码 → 取前二简首选，依然自动上屏 | §4.2：前段恒取首选 ⇒ 组合数只由后段决定，这一格与上一格是同一条代码 |
| 后二简有重码 → 显示候选 ①前+后首选 ②前+后次选…；想要①就往后打（顶首选上屏） | §4.2 组合序 + §5.1 顶码那一行（`handle_top_code` 走 `convert(prefix, 1)`，无需接线） |

---

## 2. 为什么不复用整句的 Viterbi 解码器

整句（`codetable/sentence.rs`，1026 行）做的是**同一族**的事，但三点不同让它不能直接承接本功能：

| | 整句 | 逆切分 |
|---|---|---|
| 切点 | Viterbi 在全部跨度上求最优，可能切成 1+3 / 3+1 / 1+1+2 | **固定 2+2**（音形二简的先验，是需求的核心而非实现细节） |
| 产物 | **一条**最优路径 | **一列**候选（后段重码要逐条列出，这是需求明写的） |
| 依赖 | 借道 `pinyin/rime_frost.dict.yaml` 建 6.6 万条词频表，首次解码 300–660 ms，须后台预热 | 两次 `dm.search`，**零额外数据、零预热** |
| 闸门 | `input_len > max_code_length` | `input_len == max_code_length` 且零候选 |

⇒ 强行合并会让「固定 2+2」变成给 Viterbi 加一条只允许某种切法的约束，再让它输出 N 条路径而非 1 条——
比独立写一遍贵得多，还把本功能拖进整句的词频依赖和预热复杂度里。

**但出口全部复用**，一处不新造：

- 组合区切分显示 `hf'kn` → `ConvertResult::preedit_codetable` + `sentence::SPLIT_SEPARATOR`（`'`）
- 候选构造形态 → `CodeTableEngine::decode_sentence`（`codetable/engine.rs:203`）逐字段照抄
- 配置形态 → `CodeTableSpec::sentence_input`（`wind-config/src/schema.rs:365`）的「方案级引擎固定参数」先例
- 混输下的处置 → `resolve_sentence_input` / `MixedRole::Primary`（`wind-engine/src/manager.rs:182`）

---

## 3. 触发判据

```rust
// codetable/engine.rs，紧邻 decode_sentence
fn decode_split(&self, input: &str, candidates: &[Candidate]) -> (Vec<Candidate>, String)
```

**四道门**（前三道与 `decode_sentence` 的三道门同构，逐条说明为何是这一条）：

1. **功能开启**：`opts.split_input`（方案级，出厂 false）且**切点有效**（§3.1）。
2. **恰好满码**：`input.chars().count() == self.max_code_length`。
   - 为什么不是 `>=`：超码长那一段归顶码与整句管（`handle_top_code` / `decode_sentence` 的闸门都在那里），
     两个功能抢同一个区间是整句已经踩过的坑（那篇 §11.6 顶码让位）。本功能**刻意只占 `==` 这一格**，
     与既有两者零重叠——这也是它不必和谁让位的原因。
   - 为什么不是 `<`：未满码时还有更长后继可打，切分等于替用户提前认定「这串到此为止」。
     整句的门槛注释记着真机反例：`aaw`（本意 `aawt`→「工作」）会被读成「啊啊我」。
3. **按 `split_trigger` 定的空度**：
   - `Empty`（默认）：`candidates.is_empty()`——屏幕上一条都没有。
   - `NoExact`（预留档）：`!candidates.iter().any(|c| c.is_exact_code)`——没有 `code == input` 的精确解。
     此档产出的候选**一律排在既有候选之后**且**不参与自动上屏**（§5.1），故仍不抢任何现有首选。
4. **两段都查得到**——切一半没有意义，半截结果只会让用户以为词库缺条目。

### 3.1 切点

`split_at = max_code_length / 2`，仅当 `max_code_length` 为**偶数且 ≥ 4** 时功能可用；
否则构建引擎时 `warn!` 一条并当作未开启。

> 不做成配置项：切点是「这张码表的二简在哪里结束」，由 `max_code_length` 唯一决定。
> 多给一个自由参数只会制造 `max_code_length=4, split_at=3` 这种配出来不报错、打起来全是错的状态。
> 若将来真有奇数码长的方案提出需求，那时它需要的多半也不是「切点参数」而是「多切点枚举」，
> 是另一个设计。

### 3.2 与短语的共存（已知取舍，不新增否决）

引擎看不见协调器随后叠加的短语（`handle_candidate.rs:1069-1280`），所以「零候选」是**引擎视角**的零。
若用户自定义短语恰好占了这个四码位，切分候选仍会产出，与短语一起显示、按 weight 竞争。

**这与整句的行为一致**（整句的「整串无精确解」同样不看短语），且自动上屏那一侧已有
`phrase_vetoes_auto_commit`（`handle_candidate.rs:1685`）挡着，不会替用户上错屏。
⇒ **零新增否决逻辑**。若实测发现打扰，再按 `phrase_owns_code` 的先例补一道。

---

## 4. 候选构造

### 4.1 形态（逐字段照抄整句候选，差异处注明）

```rust
Candidate {
    text: format!("{}{}", front.text, back.text),
    code: input.to_string(),           // 整串，供 decide_auto_commit 认作精确
    weight: front.weight.min(back.weight),  // 组合的可信度不高于最弱的那一段 → §4.2
    natural_order: 构造序,                  // 同权重时的末级键；base_sort=natural 的方案也靠它
    source: CandidateSource::CodeTable,
    is_split_composed: true,           // ★ 新增字段，不借 is_sentence → §4.4
    is_synthesized: true,              // 词库无此整体词条 ⇒ 自动造词据此判「值得学」
    is_exact_code: false,              // ⚠ 不置位：它不是词库里的精确解，置位会混进 cmp_exact_first 的精确档
    consumed_length: 0,                // 消费整串，不动全仓约定（取舍 #4）
    boundary: 0,                       // ⚠ 音节边界是拼音域，码表无音节语义；填切分位会让自动造词学到假真值
    comment: /* show_code_hint 时留空，切分形态走 preedit 而非 comment */ String::new(),
    ..Default::default()
}
```

### 4.2 组合与排序

```
front = dm.search(&input[..split_at], front_n)       // 默认 front_n = 1
back  = dm.search(&input[split_at..], BACK_LIMIT)    // BACK_LIMIT = 8，同 completion_hints 的取数口径
产物 = front × back，前段外层、后段内层，各自保持词典返回序
```

- **一律 `append` 到候选列表末尾**，不 `insert(0)`。默认档下列表为空 ⇒ append 即全部；
  预留档下自然排在既有候选之后。**两档共用一套代码、零分支**，这是选 append 的全部理由。
- **`weight` 取两段的较小值**：组合的可信度不高于最弱的那一段。前段恒取首选时（默认）
  `front.weight` 是常量，于是 `min` 退化为「按后段权重序」——与构造序一致。
  `natural_order` 填构造序作为同权重时的末级键，`base_sort = "natural"`（忽略权重）的方案
  也因此拿到同一个序。
- ⚠️ `append` 的位置必须在 `truncate(max_candidates)` **之前**：`handle_top_code` 以
  `convert(prefix, 1)` 取顶码首选，放到 truncate 之后会让那次调用拿回超过 limit 条候选，
  破坏 `max_candidates` 契约。同时也必须在 `is_empty` 求值之前，否则 `should_clear` 会
  在切分出候选的同时清空缓冲（用户看到候选一闪即逝）。
- ⚠️ **不要加 `.filter(|c| c.code == seg)`**。`DictManager::search` 走的是
  `CompositeDict::merge_search(code, limit, Query::Exact)`（`wind-dict/src/composite.rs:69`），
  查询本身**已经是精确的**，不会混进前缀候选；但同一条 `merge_search` 在跨层合并时会
  「同 text 取最短码」——「能」若同时在 `kn` 与某个一简位上，返回条目的 `code` 字段可能
  被换成那个更短的码。**查询口径与 code 字段不是同一件事**，按 code 过滤会误杀正确的二简候选。
  本功能只用 `text` 与 `weight`，不读段候选的 `code`。

### 4.3 为什么前段默认只取首选

取前 N 会让**同前段的变体**占满候选窗：后段有 4 个重码时，前段取 2 就是 8 条，而其中后 4 条共享一个
用户多半不想要的前段。原帖正文选的也是「前段取首选」。

旋钮 `split_front_candidates`（方案级，默认 1）留给方案作者实测——配图那种「1.很可能 2.困难」的效果
就是它设 2 的产物。**旋钮存在的意义是让这条取舍可被数据推翻，不是让用户调**。

### 4.4 为什么新增 `is_split_composed` 而不复用 `is_sentence`

`wind-engine/AGENTS.md` 有明文纪律：**「要沉底就加自己的字段，别借现成的布尔」**——
`is_prefix` 被静态短语借用、`is_fuzzy` 被用户词简拼借作沉底标记，两次都已拆出独立字段
（`is_promoted_completion` / `is_abbrev`）。

具体到 `is_sentence`，它至少有三个已知消费者语义不合：`freq_rerank` 的整句让位判据
（`is_sentence_demoted`）、`wants_codetable_split` 的 preedit 判据、若干探针的名次统计。
借用会让「整句的统计里混进切分候选」这类问题在半年后以最难查的形态出现。

新字段落点：`wind-candidate/src/candidate.rs`（`is_sentence` 邻位），`#[serde(skip)]`（同 `is_sentence_demoted`，
它是引擎到协调器的进程内标记，不进任何持久化或 IPC 载荷）。

---

## 5. 与既有机制的交互

### 5.1 自动上屏——三条通路一次都不能漏

`wind-engine/AGENTS.md` 的硬约定：任何新的上屏/否决判据必须同时接
**`convert` 满码 / `recheck_auto_commit` 显示态复评 / `handle_top_code` 顶码**三条通路。

好消息是本功能**三条都不用改判据**，只要候选进了列表：

| 通路 | 位置 | 为什么自动正确 |
|---|---|---|
| `convert` | `codetable/engine.rs:481` | `decide_auto_commit` 的判据是「恰一个 `c.code == input && !c.is_scope_filtered` 的候选」。切分候选 `code == input` ⇒ 后段唯一时恰好一条 ⇒ 自动上屏；后段有重码时多条 ⇒ 不上屏、出候选窗。**这正是原帖前三条规则的完整实现，零额外判据。** |
| `recheck_auto_commit` | `codetable/engine.rs:542` | 同一个 `decide_auto_commit`，收的是显示态候选 |
| `handle_top_code` | `codetable/engine.rs:552` | 闸门是 `input.chars().count() > max_code_length`，与本功能的 `==` 不重叠。而它的取首选走的是 **`self.convert(&prefix, 1)`**（`engine.rs:576`）⇒ prefix 恰好是满码长，切分在那次 `convert` 里照常触发，顶码天然取得到 |

✅ 已核实（2026-09-19）：`handle_top_code` 内部是 `self.convert(&prefix, 1).ok().and_then(|r| r.candidates.first()…)`，
**第四条通路不必接**。原帖规则三（「想要①就直接往后打」）由此自动成立：5 码时顶码取前 4 码的
切分首选上屏、第 5 个字母留作余码续打。

⚠️ 但有一个**前提失效**的情形要记住：`handle_top_code` 第二道闸是「`sentence_input` 开启则顶码整体让位」
（`engine.rs:561`）。方案若同时开了 `sentence_input` 和 `split_input`，规则三随顶码一起落空——
那时超码长区间归整句管，是整句那篇已定的取舍，不是本功能的缺陷。**S3 的 warn 要覆盖这一组合。**

预留档 `NoExact` 下切分候选**不参与自动上屏**：此时列表里还有别的候选，`decide_auto_commit` 的
「恰一个」判据本就不成立，无需额外代码——但要有测试锁住，否则将来有人放宽那个判据会静默破坏。

### 5.2 满码空码清空（`clear_on_empty_max`）

`codetable/engine.rs:492` 的 `should_clear = is_empty && ...`。
**切分候选必须在 `is_empty` 求值之前入列**（即 `engine.rs:486` 那行之前），
这样 `is_empty` 自然变 false、清空自然不触发，**不需要在清空判据里加任何条件**。

这条顺序是硬要求：写反了就是「切分出候选了但缓冲被清空」——用户看到候选一闪即逝。
协调器侧的第三道门 `clear_blocked_by_candidates`（`handle_candidate.rs:254`）以最终列表复查，
也会跟着正确，无需改动。

### 5.3 组合区显示 `hf'kn`

- 引擎侧：`ConvertResult::preedit_codetable` 填 `input[..split_at] + SPLIT_SEPARATOR + input[split_at..]`。
  该字段当前由整句独占，两者的闸门互斥（`==` vs `>` 码长），不会互相覆盖。
- 协调器侧：`wants_codetable_split`（`handle_candidate.rs:5548`）当前写死
  `c.is_sentence && c.source == CodeTable`，**须加上 `|| c.is_split_composed`**。
  这是本功能在协调器里**唯一**必须改的一行判据。
- ★ 该分支必须留在 `preedit_split_body.is_empty()` 守卫**之前**（`handle_candidate.rs:2380` 已有注释）：
  码表方案下拼音拆分形态恒空，守卫会直接 return 原始码，后面一条都走不到。

### 5.4 候选排序里的沉底层

预留档 `NoExact` 要求切分候选排在既有候选之后。`append` 只保证**入列时**在后面，
协调器的 `candidate_display_order` 会无条件重排全部候选（AGENTS.md：「候选排序必须落到 weight」），
纯靠 append 的顺序留不住。

⇒ 在 `candidate_display_order` 的层级链里加一层 `is_split_composed asc`，位置在
`is_fuzzy` / `is_partial` / `is_prefix` 那组之后、`weight desc` 之前。

> 默认档下这一层是空操作（列表里只有切分候选），它**只为预留档存在**。
> 这也是为什么预留档值得在同一轮做掉：判据的位置一旦定错，默认档下不会有任何症状。

### 5.5 词频与自动造词

- **词频记账**：`handle_candidate.rs:320-351` 只给「消费整串」的候选记频，切分候选 `consumed_length = 0`
  ⇒ 会记一条 `(hfkn, 很可能)`。**这是期望行为**：打过一次，下次 `hfkn` 直接有，切分从此不必再算。
- **自动造词**：`is_synthesized = true` ⇒ `learn_phrase_on_commit`（`handle_candidate.rs:3167`）判它「值得学」。
  同上，期望行为。
- ⚠️ 两者都意味着**切分产物会进用户词库**。若实测发现误切分被固化（用户选错一次就永久多一条脏词条），
  再考虑给 `is_split_composed` 在这两处做豁免。**先不预防**——整句走的是同一条路且未见此类反馈，
  且豁免掉就失去了「打过一次下次直接有」这个实打实的收益。

### 5.6 混输方案

**与整句不同，本功能在混输下会真正触发**——这是必须写下来的差异：

| 输入 | 混输走哪条路 | 整句 | 逆切分 |
|---|---|---|---|
| 码长内（≤4 码） | `primary.convert` | 门槛（>4）不满足 ⇒ 不产 | **门槛（==4）满足 ⇒ 会产** |
| 超码长（>4 码） | `convert_overflow`，不经主引擎 | 不产 | 门槛不满足 ⇒ 不产 |

⚠️ **「零候选」在混输下是「码表侧零候选」**，不是合并后的零：`decode_split` 跑在
`CodeTableEngine::convert` 内部，它看到的 `candidates` 只有码表自己那一路，拼音候选还没汇进来。
所以混输下的实际触发面是「**五笔四码空码**」——这在混输里相当常见（拼音那一路照样有解）。
切分候选随后回到 `MixedEngine` 与拼音候选同场竞争排序。

⇒ **已实现的处置：照 `MixedRole::Primary` 的先例，取混输方案自己的 `[engine.codetable]
split_input` 声明，不继承 `primary_schema`**（`resolve_split_input`，有单测锁）。
不继承的理由比整句更硬：整句在混输下压根不会触发，配错了只是没反应；逆切分**会真正生效**，
`wubi86` 单用时开着合适不等于 `wubi86_pinyin` 下也合适——后者多了一路拼音候选，是完全不同的
候选环境。

**没有加混输 warn**（整句有一条）：整句那条 warn 说的是「配了也不会触发」，而逆切分会照常
工作，没什么可警告的。它在混输下好不好用，是方案作者实测的事。

混输下给切分候选一个**明确的档位**（而不是任其按 weight 与拼音竞争）是独立的一步，不在本轮。

---

## 6. 配置

### 6.1 三个键，全部是**方案级引擎固定参数**

落在 `CodeTableSpec`（`wind-config/src/schema.rs`，`sentence_input` 邻位），
**不进全局 `schema.codetable`、不进 `config_schema.rs` 的 REGISTRY、不进设置页**：

```toml
# 某音形方案的 .schema.toml
[engine.codetable]
max_code_length = 4
split_input = true              # 逆切分自动切分。出厂 false
split_front_candidates = 1      # 前段取几条（§4.3）。0 或缺省 = 1
split_trigger = "empty"         # "empty"（默认）| "no_exact"（§3 预留档）
```

**为什么是方案属性而非用户偏好**（同 `sentence_input` 的论证，逐字成立）：
一张码表逆切分出来是什么效果，取决于它有没有成体系的二简、二简空间留了多少余量——
这是编码方案的结构事实，不是「喜不喜欢」。

**但不限定方案类型**：音形类码表是它的典型受益者（编码空间论证正是为它成立的），
别的码表开了未必没用，只是表现随编码结构而异——五笔的二简位被简码字占着，切出来的
多半是「简码字 + 简码字」而不是「二简词 + 二简字」，好不好用得打过才知道。
所以代码里**没有任何方案类型判据**，只有一个方案作者/用户可以自己开的开关：
除了方案作者在 `.schema.toml` 里声明，用户也能经设置页写 `schema_overrides/{id}.toml`
的同名 `[engine.codetable]` 段自行覆盖（`read_schema` 深合并，已有通路）。

**连带后果（有意为之）**：不改全局配置 ⇒ 设置仓 `../wind-setting` 的
`settings_manifest.toml` 与文档站的配置项清单**一行都不用动**，跨仓一致性风险为零。

### 6.2 装配

`CommitOptions`（`codetable/engine.rs:57`）加三个字段，在 `manager.rs:4735` 的码表分支接线，
`split_input` 走 `resolve_split_input(mixed_role, ...)`（仿 `resolve_sentence_input`，§5.6）。
另两个直接取方案声明。

⚠️ 同步核对 `reload_from_config` 是否覆盖这三个键——否则改方案文件要重启才生效。

---

## 7. 编码空间论证的复核（S0，可与 S1 并行）

flypy 的论证是**在小鹤音形上**成立的；本仓需要自己的数据，否则功能开出去才发现某方案上
四码空码位少得可怜（收益近零）或多得离谱（全是噪声）。

**要测的三个数**（拿一份真实音形码表，需求方提供 `.wpkg` 或词库文件）：

| 指标 | 怎么算 | 判据 |
|---|---|---|
| 四码空码率 | 枚举 26⁴ 中符合该方案 `input_chars` 的串，统计 `dm.search` 为空的比例 | 太低 ⇒ 功能没机会触发 |
| 可切分率 | 空码串中，前后两段**都**查得到的比例 | 这是功能的实际覆盖面 |
| 后段重码分布 | 可切分串的后段候选数直方图 | 决定「有多少比例能自动上屏」——原帖收益论证的核心 |

探针：`crates/wind-engine/tests/codetable_split_probe.rs`（`#[ignore]`，依赖 `build_dev`
下的构建产物词库）。换一张码表只改 `PROBE_DICTS` 里的路径与码元集即可出同一份报告。

```text
cargo test -p wind-engine --test codetable_split_probe -- --ignored --nocapture
```

⚠️ 它量的是**编码空间的形状**，不是切出来的词对不对——后者要人读，故探针只抽样打印
实例供人工判读，不自动判分。拿词库条目回测切分正确率是整句那篇 §8 点名的假绿。

### 7.1 五笔 86 的实测（2026-09-19，极点主库）

| 指标 | 数 | 占比 |
|---|---:|---|
| 枚举四码串（码元 `a-y`，25⁴） | 390,625 | |
| 空码 | 318,830 | **81.62%** 的码空间 |
| 其中可切分（前后两段都查得到） | 308,027 | **96.61%** 的空码 |
| 后段唯一（能四码自动上屏） | 287,816 | **93.44%** 的可切分 |

后段重码分布：1 → 287,816；2 → 20,211；**≥3 → 0**。

**触发面极大，但切出来的几乎全是「两个不相关的简码单字」**，抽样实录：

```
ab'yb → 节 + [离]      ae'aq → 菜 + [区 获]    ag'gr → 七 + [珠]
ai'jn → 东 + [电]      bb'kc → 子 + [吧]       bm'bq → 出 + [隐 聊]
```

### 7.2 ⇒ 这组数正好说明「它为什么是音形方案的功能」

**决定切出来是词还是字的，是二简位上放着什么**：

- 音形的二简空间 676 里，拼音声韵组合只占约 400，**余下的码位放得下简词** ⇒ 切出来是
  「二简词 + 二简字」（`hf`(很可) + `kn`(能)），是一个真实的语言单位；
- 五笔的二简位是**一二级简码，全是单字**，没有词的位置 ⇒ 切出来必然是两个字的机械拼接。
  五笔在四码上的空码率高达 81%，那不是「等着被利用的编码空间」，那就是**空码**。

⇒ 对五笔类纯字形码表，开启本功能的实际效果是：**八成的四码空码会冒出一条多半读不通的
候选**。功能不禁止这么用（不设任何方案类型判据），但方案作者该先跑一遍这个探针再决定。

### 7.3 ⚠️ 五笔类码表开启时的一个具体后果：顶码会顶出垃圾

`top_code_commit` **出厂是 true**。没开逆切分时，四码空码串的 `handle_top_code` 取首选取到
空字符串，由上层的短语/命令兜底（或不顶）；开了之后它取到的是切分候选 ⇒ **用户打错码、
继续往下打时，那条拼凑出来的「节离」会被自动顶上屏**。

这不是缺陷（顶码本就是「前 N 码首选上屏」，切分候选就是首选），但它把「打错码」的代价从
「什么都没发生」变成了「上屏了两个不相干的字」。音形方案下这一条是**收益**（原帖规则三
要的正是它），五笔方案下是**风险**。同一条机制，两张码表上的价值相反——这就是为什么它是
方案级开关而不是全局开关。

### 7.4 仍缺音形侧的对照数

上面只有五笔一张表。**音形方案的同一组数仍需要一份真实音形码表**（`.wpkg` 或
`.dict.yaml`）才能算出来——把路径填进 `PROBE_DICTS` 即可。预期它与五笔的差别应当出现在
「切出来的是不是词」上，而不一定在三个百分比上。

---

## 8. 分阶段实施

| 阶段 | 内容 | 验收 |
|---|---|---|
| **S0 论证复核** | §7 三个指标的离线探针 | 拿到真实音形码表的三个数；覆盖面低于预期则回头找需求方重定范围 |
| **S1 引擎** | ① `Candidate::is_split_composed`；② `CommitOptions` 三字段；③ `decode_split`（§3 三道门 + §4 构造）；④ `convert` 内接线（**`is_empty` 求值之前**）；⑤ `preedit_codetable` 填充 | `cargo test -p wind-engine`：满码零候选出组合候选 / 未满码不切 / 超码长不切（让给整句与顶码）/ 两段缺一不产 / 后段唯一时 `should_commit` 为真 / 后段重码时为假且候选按后段权重序 / **5 码时 `handle_top_code` 顶的是切分首选**（原帖规则三，§5.1）/ 关闭时零行为变化 |
| **S2 协调器** | ① `wants_codetable_split` 加判据（§5.3）；② `candidate_display_order` 加沉底层（§5.4） | `cargo test -p wind-coordinator`：高亮切分候选时组合区显示 `hf'kn`、高亮别的候选时显示原串；`no_exact` 档下切分候选恒在既有候选之后 |
| **S3 配置与混输** | ① `CodeTableSpec` 三字段；② `manager.rs` 装配 + `resolve_split_input`；③ 奇数/过短码长的 `warn!` 降级；④ 混输 warn（§5.6）；⑤ `reload_from_config` 核对 | 方案 TOML 开关生效；混输方案声明后 warn 且默认档无效果；全局配置与设置仓零改动（`git diff` 自证） |
| **S4 收尾** | ① 本文「现状」并入 engine-candidate-pipeline.md §3 与 §10 对比表；② `.omc/feedback-todo.md` 的 C0-5 勾选；③ 论坛 t11 回帖 | `cargo fmt` 后逻辑与格式**分开提交**；全量测试绿 |

**每阶段独立提交**，提交只用显式路径（本仓多会话共仓，`git add -A` 禁用）。

### 8.1 怎么开启（设置页没有这个开关，测试前必读）

三个键都是方案级引擎固定参数，**设置页不暴露**（同 `sentence_input`）。两条开启路径：

**① 用户目录的方案覆盖（推荐，不动安装目录）**

```
%APPDATA%\WindInput\schema_overrides\{方案id}.toml      # 正常安装 release
%APPDATA%\WindInputDev\schema_overrides\{方案id}.toml   # dev 变体
<exe目录>\userdata\schema_overrides\{方案id}.toml       # 便携模式
```

```toml
[engine.codetable]
split_input = true
# split_front_candidates = 2   # 想看原帖配图那种「前后都列」的效果时再开
# split_trigger = "no_exact"   # 想让它在「有前缀候选但无精确解」时也切
```

**② 方案自带（方案作者发布时声明）**：同样的段写进 `.schema.toml`，用户在设置页会看到
它标着「方案自带」。

⚠️ **改完要重启服务**。热重载走的是 `Coordinator::reload_user_config` 的 `schema_dirty`
判据，那个判据只比较 `config.toml` 的各段，**看不见 `schema_overrides/*.toml` 的改动**；
而 `switch_schema` 只切活跃方案、不清引擎缓存（`ensure_loaded` 命中已建好的那个）。
⇒ 只有 `reload_from_config` 的 `engines.clear()` 能让新值进来，而它要由 config 变更触发。

⚠️ 若方案的 `max_code_length` 不是偶数或小于 4，开了也不生效，日志里有一条 `warn`
说明原因（切点取码长的一半，奇数码长无解）。

### 8.2 真机验证的判据（靶机跑不了 GUI 自动化，须人工验）

给需求方/自己的验证清单，每条都是可判真假的：

1. 开关关闭时，四码空码的行为与当前**逐字相同**（组合区不清空、候选窗不出现）。
2. 开关开启 + 一个已知的四码空码串 → 候选窗出现组合候选，组合区显示 `xx'yy`。
3. 后段唯一 + `auto_commit_at_full = true` → 四码打完直接上屏，无候选窗。
4. 后段重码 → 候选窗列出，继续打第 5 个字母 → 首选上屏、第 5 个字母进缓冲（原帖规则三）。
5. 上屏后再打同一串 → 这次是词库/词频里的整条，不再经切分（§5.5 的期望行为）。

---

## 9. 风险与未决

| # | 事项 | 状态 |
|---|---|---|
| 1 | ~~`handle_top_code` 能否取到切分候选~~ | **已核实（2026-09-19）**：走 `self.convert(&prefix, 1)`，天然取得到。但 `sentence_input` 同开时顶码让位 ⇒ 规则三落空，S3 的 warn 要覆盖（§5.1） |
| 2 | ~~`dm.search` 是否恒精确~~ | **已核实（2026-09-19）**：`Query::Exact`，查询精确；但返回条目的 `code` 会被跨层「同 text 取最短码」改写 ⇒ **不得按 code 过滤**（§4.2） |
| 3 | 真实音形方案的空码率/可切分率（§7） | **S0 阻塞项**：需求方提供一份真实音形码表 |
| 4 | 切分产物进用户词库是否会固化误切（§5.5） | 先不预防，实测后再定 |
| 5 | 混输下的档位归属（§5.6） | 本轮不做，留独立一步 |
| 6 | 逆切分·手动（切分键）与既有 `manual_separator_key` 的关系 | 本轮不做（取舍 #2），做之前要先定两者是同一根轴还是两个功能 |

---

## 10. 实现纪要

### 10.1 ★ 三条上屏通路一条都没改 —— 「进列表」即得

计划里最担心的是「上屏三通路」（AGENTS.md 的硬约定：新判据必须同时接 `convert` /
`recheck_auto_commit` / `handle_top_code`）。实际**一条都没动**，因为切分候选的 `code == input`：

- `decide_auto_commit` 的判据是「恰一个 `code == input && !is_scope_filtered` 的候选」。
  后段唯一 ⇒ 恰好一条 ⇒ 自动上屏；后段重码 ⇒ 多条 ⇒ 出候选窗。**原帖前三条规则，零代码。**
- `handle_top_code` 取首选走 `self.convert(&prefix, 1)`，prefix 恰好是满码长 ⇒ 切分在那次
  convert 里照常触发。**原帖规则三，零代码。**
- `should_clear` 读的是同一个 `is_empty` ⇒ 切分候选一入列它自然变假。

★ 这不是巧合，是**选对了候选身份**的结果：把切分产物做成「一条 `code` 等于整串、消费整串的
普通码表候选」，它就自动继承了码表候选的全部既有待遇。反过来，若当初按计划初稿走
「引擎备池 + 协调器收口」（仿 `completion_hints`），这三处就要各接一次线——备池里的东西
不在 `candidates` 里，上面三条判据一条都看不见它。

**整句才是对的先例，`completion_hints` 不是**：判据是「它参不参与上屏决策」。
补全提示是兜底展示，可以不出；切分候选要能自动上屏、要能被顶码取到，那就必须是一等候选。

### 10.2 沉底层加进 `candidate_display_order` 是安全的（那条告诫不适用）

`place_english_after_common_exact` 的文档明确劝阻「往 `candidate_display_order` 加比较键」。
但它否决的是**想对某些候选对表态、对另一些返回 `Equal`** 的比较器——那不构成全序，
`sort_by` 在偏序下的结果未指定。

`is_split_composed` 这一层是按一个布尔**分区**，是合法全序，且两条非切分候选之间恒 `Equal`
⇒ 可证明不改变任何既有次序。守门测试 `split_composed_sinks_below_ordinary_candidates`
把两个方向都锁住了（切分候选权重 9999 仍沉底 / 两条普通候选之间高权重仍在前）。

### 10.3 两条告警落在 `CodeTableEngine::new`，不在 manager

奇数/过短码长（切点无解）与「和整句同开」这两条，都需要**同时**握着 `max_code_length` 与
两个开关才能判——那只有 `new` 里有。落在 manager 的装配处就要把三个值都传过去，
且 `new` 的其它调用点（测试、将来的构造路径）会绕过告警。

前者不只告警，还**就地把 `opts.split_input` 置 false**：留给 `split_at()` 每次按键静默返回
`None` 的话，配置者只会观察到「开了没反应」，拿不到任何线索。

### 10.4 测试词库的一个坑：别把 `no_exact` 的道具放进公共集

`SPLIT_ENTRIES` 里一度放了 `hfknq`（「甲」，`hfkn` 的五码扩展），本意是给 `no_exact` 档
造「有候选、无精确解」的场景。结果它给**每一个**默认档用例凭空添了一条前缀候选，
默认档的「有候选就不切」随即把 8 个基本盘用例全挡掉。

⇒ 该条目移出公共集，只由需要它的那一条用例经 `extra` 注入（`PREFIX_ONLY_ENTRY`）。
**公共夹具里的每一条都会参与每一个用例的判据**——尤其当判据本身就是「有没有候选」时。

---

## 11. 当前状态

| 阶段 | 状态 |
|---|---|
| S0 论证复核 | **五笔侧已做**（§7.1 三个数 + 结论：触发面 81.62%，但切出来是两个简码单字的拼接）。**音形侧仍缺**——需要一份真实音形码表填进 `PROBE_DICTS` |
| S1 引擎 | **已完成**：字段 / 配置载体 / `decode_split` / `convert` 接线 / `preedit_codetable` / 两条构建期告警 |
| S2 协调器 | **已完成**：`wants_codetable_split` 判据、`candidate_display_order` 沉底层 |
| S3 配置与混输 | **已完成**：`CodeTableSpec` 三字段、`manager` 装配、`resolve_split_input`（不继承 primary）。全局配置与设置仓**零改动** |
| S4 收尾 | 文档本轮已写；`.omc/feedback-todo.md` 的 C0-5 与论坛回帖待功能验证后再动 |

**测试 18 条**（含 1 条 `#[ignore]` 探针）：`codetable/engine.rs` 13 条（基本盘 / 字段约定 / preedit / 关闭零变化 /
闸门只占 `==` / 两段缺一 / 默认档让位 / `no_exact` 档含反向对照 / 唯一即上屏 / 重码不上屏 /
顶码顶切分首选 / 前段多取 / 奇数码长关闭 / 与整句分区间）、`manager.rs` 1 条
（`resolve_split_input` 不继承，含「两开关互不串味」）、`handle_candidate.rs` 2 条
（组合区判据含反向对照 / 沉底层含两条反向对照）、
`tests/codetable_split_probe.rs` 1 条编码空间探针（`#[ignore]`，依赖 `build_dev` 词库）。

**仍待做**：音形码表的对照数（§7.4）；真机验证（清单见 §8.2）；混输下的档位归属。
