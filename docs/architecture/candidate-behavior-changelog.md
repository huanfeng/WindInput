# 候选行为变更记录

> 用户反馈「某个词以前排第 1、现在排第 2」「以前会自动上屏、现在不会」时，先查这里。
> 每条记录一次**用户能感知到的**候选行为变化：哪些输入受影响、前后对比、由什么条件触发、
> 用哪个配置能回到旧行为、哪个测试钉住了新行为。
>
> **什么时候要写**：改动影响候选的**有无、先后、条数、上屏时机**（自动上屏 / 顶码 / 满码清空），
> 涉及引擎（`wind-engine`）、候选模型（`wind-candidate`）、协调器装配与排序
> （`wind-coordinator/src/handle_candidate.rs` 等）、词频重排、英文 / 短语 / 联想混入。
> 纯重构（输出逐条不变）、仅修崩溃、仅改日志不用写。
>
> **怎么写**：与改动同一分支提交；新条目加在最上面；提交 hash 写合入 main 的合并提交
> （rebase 后会变，合入后再填）。排序口径本身仍以 [candidate-sorting-rules.md](candidate-sorting-rules.md)
> 为准，本文件只记「哪次改了什么」。

条目格式：

```
## 日期 · 标题（版本）
- 合入：<合并提交>；范围：<方案类型 / 模式>
- 变化：<输入> 前：… → 后：…（每类各举一两个真实输入）
- 触发条件：<出厂即生效 / 需开某开关 / 某配置组合>
- 回退旧行为：<配置键 = 值>，没有则写「无（设计变更）」
- 钉住的测试：<测试文件::用例>
- 来源：<论坛 tNNN / GH#NNN / 内部>
```

---

## 2026-11-07 · 联想态可不挂占位组合（0.131）

- 合入：2aeb737c..f96bc274（快进合入 main，无合并提交）；范围：Windows TSF，仅 compat 规则配了 `assoc_placeholder = "none"` 的应用（出厂：Excel、WPS 表格）
- 变化：
  - 联想窗出现时，宿主里不再有占位空格 / 零宽空格，表格自己的单元格提示不被干扰。
  - 联想窗位置钉在联想出现处，不随宿主上报的远处光标飘走。
  - 点别的单元格（鼠标）或挪光标，联想窗立即消失；前：要等自动隐藏计时到期。
- 触发条件：该应用的 compat 规则 `assoc_placeholder = "none"`；其它应用不受影响（不写则跟随 `composition_placeholder`）
- 回退旧行为：把该应用的 `assoc_placeholder` 改回 `space` / `zwsp` / `blank`，或删掉这一行
- 钉住的测试：`wind-coordinator` `handle_assoc::tests::assoc_none_mounts_no_placeholder_but_keeps_the_session_flag` /
  `bare_assoc_ignores_far_caret_reports_for_anchoring` / `bare_assoc_exits_when_user_moves_the_caret` /
  `bare_assoc_released_by_passthrough_realigns_the_session_cache`；`wind-bridge` `server::tests::assoc_session_flag_only_on_plain_commit`
- 来源：内部（WPS 表格 / Excel 靶机实测）

## 2026-11-06 · 码表整句：语法模型、词典组词、N-best（0.131）

- 合入：14a4e9b0；范围：码表方案，仅方案开了 `[engine.codetable] sentence_input = true`
- 变化：
  - 整句首选更准：挂上语法模型并能把码表没收的词逐字组出来。`qjkhlgw` 前：旬中国人 → 后：我是中国人。
    3000 句评测首选：词组最短码打法 56.8% → 90.0%，逐字最短码打法 7.7% → 89.4%。
  - 用户自造词（拼音库里没有的）在整句里不再总被拆成单字。
  - 可露多条整句：`sentence_count = 2` 时 `qjkhlgw` 出「我是中国人 / 旬中国人」两条；配了 `sentence_cycle_key` 可轮换。
    组合区切法跟着高亮的整句走（`q'j'khlg'w` ↔ `qj'khlg'w`）。出厂 1/1 下组合区行为不变。
  - 码表方案下展开态 Tab 切分类不再被 `schema.pinyin.sentence_cycle_key = "tab"` 挡住（码表看自己那把键）。
- 触发条件：语法模型与组词在开了整句的方案上出厂即生效；多条整句需调 `schema.codetable.sentence_count`
- 回退旧行为：`schema.codetable.sentence_grammar_weight = 0` 关语法模型；组词无开关（设计变更）
- 钉住的测试：`wind-engine` `codetable::sentence::tests::composes_lexicon_word_across_char_units` /
  `user_word_missing_from_lexicon_gets_floor_frequency` / `grammar_picks_rank_one_and_plain_nbest_fills_the_rest`，
  `codetable::engine::tests::sentence_nbest_shows_count_and_fills_pool`；
  `wind-coordinator` `tests/codetable_sentence_nbest.rs`
- 来源：内部

## 2026-11-06 · 整句备选不再被第 1 名的同文切分挤掉（0.131）

- 合入：bdf132b8；范围：全拼 / 双拼主路径，仅 `sentence_count > 1`
- 变化：N-best 原取「前 k 条路径」再按文本去重，同一句的不同切分会占掉名额，备选就露不出来。现改为解码时按已拼文本去重，前 k 条即 k 条不同的句子。
  例：`xianchuanwazi`、露 2 条，选过一次「西安穿袜子」（学成整串临时词）后再输 → 前：西安穿袜子 / 先传 …（先穿袜子消失）；后：西安穿袜子 / 先穿袜子。
  `woshizhongguoren`、露 2 条 → 前：没有备选；后：我是中国人 / 卧室中国人。
- 触发条件：需把智能组词显示条数调到 ≥ 2；出厂 (1, 1) 不变
- 回退旧行为：无（修复）
- 钉住的测试：`wind-engine` `pinyin::tests::nbest_alternate_survives_after_top_sentence_is_learned`（旧逻辑下红）
- 来源：内部

## 2026-11-06 · 新增中英 / 中数切换处空格与英文后智能标点（0.131）

- 合入：6f8150db、57a94412；范围：中文主路径与各 overlay 的上屏前缀；中文模式空缓冲下汉字后的数字键；中文标点状态下的标点转换
- 变化：
  - 新开关 `input.spacing.cjk_latin` / `cjk_digit`（默认都关）：汉字与英文、汉字与数字**切换处**补空格，补在切换后那一笔前面。
    例：五笔「工」后临英 `Python` → ` Python`，再打「工」→ ` 工`；汉字后打 `3` → ` 3`（主键盘、小键盘都算）。
    标点永不前补；连续的英文之间不管；用户自己按过空格、鼠标点过别处、或输入法不知道光标前是什么时不补。
  - `commit_space`（英文方案 / 临英两份）仍是上屏后立即补尾随空格，之后接中文看到的是空格，不再重复补。
    临英那份新增覆盖中文方案主路径里的英文词库候选（码表 / 拼音的英文混入）：例：五笔 `git`（空格）→ 前：`git` → 后：`git `（开着时）。
  - 新开关 `input.punct.smart_after_latin`（默认关）+ `latin_smart_list`（出厂 `./_:@`）：英文词后的这些标点直出半角。
    例：临英 `Node` + `.` → 前：`Node。` → 后：`Node.`；`,` 不在出厂列表，仍出 `，`。
- 触发条件：均需手动开启（英文候选那条需已开临英的 `commit_space`）
- 回退旧行为：关掉对应开关；中英空格可按应用关闭（compat `auto_spacing = false`）
- 钉住的测试：`wind-coordinator/tests/auto_spacing.rs`、`latin_smart_punct.rs`；判据单测 `wind-punct` `spacing::tests`
- 来源：内部（设计见 `docs/design/auto-spacing-and-latin-smart-punct.md`）

## 2026-11-06 · 开整句 N-best 时第 1 名合成整句不再掉到第 2 名之后（0.131）

- 合入：d203d1e6；范围：全拼 / 双拼主路径，仅 `sentence_count > 1`
- 变化：第 1 名是多词拼出的整句时，第 2 名整句曾被当作「精确整词」，第 1 名因此降到它之下，块外排序后名次倒挂。
  例：`xianchuanwazi`、露 2 条 → 前：先穿袜子 / 西安穿袜子；后：西安穿袜子 / 先穿袜子。露 3 条 → 前：先穿袜子 / 西安川娃子 / 西安穿袜子；后：西安穿袜子 / 先穿袜子 / 西安川娃子。
  第 2 名与用户 / 临时词同文时它算词条，第 1 名照旧让位；第 1 名本身是词典词时（如 `nihao`→你好）不受影响。
- 触发条件：需把智能组词显示条数调到 ≥ 2；出厂 (1, 1) 不变
- 回退旧行为：无（修复）
- 钉住的测试：`wind-engine` `pinyin::tests::nbest_alternate_sentence_does_not_demote_top_sentence`（旧判据下红）、`nbest_alternate_merged_with_user_word_still_demotes_top_sentence`
- 来源：内部

## 2026-11-04 · `$SS`/`$AA` 重码折叠只认同码精确候选，拼音域普通候选恒让位（0.131）

- 合入：92fa87c2；范围：全拼 / 双拼 / 混输主路径（码表方案不变）
- 变化：组全码命中时，普通拼音候选（整句、简拼词、简拼回退补条）不再算重码。
  例：`js` 既是 `$SS` 组又有简拼词「技术」→ 前：折成组名，选了才进组；后：直接展开成员，同码简拼词仍在列表。
  码表域行为不变：同码码表精确词（混输下同码码表词）、精确码短语、同码精确命令、多数组同码，仍折叠为组名候选。
- 动机：拼音型方案下组码天然是简拼形态（声母缩写 / 双拼序列），0.129 之后任意声母组合都稳定产出同码候选，旧判据「非前缀补全 + 消费整串」让折叠成常态，短语全码自动展开失效。组码是精心设计的缩写，命令短语优先于泛化拼音预测。
- 触发条件：出厂即生效
- 回退旧行为：无（设计变更）
- 钉住的测试：`wind-coordinator/tests/group_expansion_rule.rs::pinyin_abbrev_words_do_not_fold_group`（旧判据下该用例红）；码表域用例未动，全照旧
- 来源：内部

## 2026-11-02 · 方案级每页候选数 `[candidate] per_page`（0.130）

- 合入：869e0c0d；范围：所有方案的主输入路径分页（翻页、数字键选词、展开网格行宽）
- 变化：方案文件或 `schema_overrides/{id}.toml` 的 `[candidate]` 写 `per_page = N` 后，该方案作为活跃方案时每页 N 条，
  切走即回到全局 `ui.candidate.per_page`。例：全局 7，五笔声明 `per_page = 5` → 五笔每页 5 条、拼音仍 7 条。
  临拼 / 快捷等 overlay 模式：`per_page_extended > 0` 不受影响；`= 0` 时跟随五笔生效的 5。
  值须为不小于 1 的整数，写错（0 / 负数 / 非整数）整条忽略并出现在方案降级提示里，不会变成 7。
- 触发条件：方案显式声明才生效；出厂方案均不声明，默认行为不变
- 回退旧行为：删除方案里的 `per_page` 行
- 钉住的测试：`wind-coordinator/tests/schema_ui_fold.rs`；`wind-config` 的 `candidate_generic_*`、`scheme_fold_read_gates`
- 来源：内部（通用折叠机制的首个使用者，见 `docs/design/schema-scoped-behavior.md` §3.4）

## 2026-11-01 · `$SS`/`$AA` 全码有重码时不自动展开；`$SS` 支持内嵌 `$CC` 成员（0.130）

- 合入：待合并；范围：码表 / 拼音 / 混输主输入路径（临拼、临英、特殊模式等 overlay 暂不变）
- 变化：
  - 全码匹配且同码**还有别的候选**（码表词、别的短语、另一个组）：前 → 组成员全部炸开，
    挤满候选页；后 → 折成一条组名候选（`is_group`），选中后才展开，且只列该组成员。
    例：`uu` 既是 `$SS("括号", …)` 又有码表词「乙」，现在候选是「乙 / 括号」。
  - 全码匹配且**无重码**：仍自动展开（不变）；前缀匹配：仍折成组名（不变）。
  - `$SS("组", "文本", $CC("显示名", 动作...))`：嵌入的 `$CC` 成员此前在候选里被静默丢弃，
    现在出为独立命令候选，选中只执行该成员的动作链。词库文件与短语库两条通路都生效。
  - 命令候选按「显示名 + 动作」去重，不再被同名普通候选吞掉。
  - 重码折叠下选中组进入展开态后，Backspace 先退回折叠视图（输入码不变），再按才删码；
    无重码、重查仍自动展开时不吞键，照常删码。Esc 不变（取消整个输入）。仅主路径。
    开关 `input.cmdbar.backspace_collapse_group`（默认 true，false = Backspace 直接删码）。
- 触发条件：出厂即生效
- 回退旧行为：无（设计变更）
- 钉住的测试：`wind-coordinator/tests/group_expansion_rule.rs`；
  `wind-phrase` 的 `test_cmdbar_array_embedded_command_members`、`dict_value_group_carries_command_members`
- 来源：内部

## 2026-10-31 · 简拼整长匹配之后逐级补短前缀词组与首声母单字（0.129）
- 合入：待合入；范围：拼音方案、纯简拼 / 混合简拼
- 变化：`tymh` 前：只有「头也没回」1 条 → 后：「头也没回」之后依次是 `tym`/`ty` 词组（他也没、同样、体验…），最后是首声母单字（他、它、她…）。`bxzd` 前：只有 4 字词 → 后：4 字词之后补「不想知」「必须」等前缀词组，再补「被、吧、把…」。整长匹配仍在最前，首选不变。
- 条数：三字及以上切点每层最多 50 条、两字切点 20 条（内部配置 `AbbrevFallbackConfig`，参考微软拼音不限、豆包 3 字不限 / 2 字 8 条）；首声母单字不设小限额，同直接打首字母；两个字母的简拼（`zd`）也补首声母单字。
- 触发条件：出厂即生效；输入须有「成不了音节的字母」（纯全拼不受影响）。首声母单字只在整串是纯简拼形态时补（`buxzd` 首音节已打全，不补 `b` 的单字）。
- 混输：表现不变。简拼出厂关（`schema.mix.enable_pinyin_abbrev`）；开启后码长内的部分候选仍被 `pinyin_partial_candidates` 过滤，回退只在超码长时可见。
- 实现注记：回退补的首声母单字不吃裸声母单字提权、权重置 0，否则候选上限收紧（约 290）时词组会先被截掉。
- 回退旧行为：`[schema.pinyin.abbrev] enabled = false` 关简拼；仅回退本条无配置，属设计变更
- 钉住的测试：`wind-engine/tests/pinyin_abbrev_short_fallback.rs`
- 来源：论坛 t371（A2-100）；对照微软拼音「整长匹配 → 词组 → 单字」

## 2026-10-24 · 拼音误读表：召回读错的词并提示正确拼音（0.128）

- 合入：feat/pinyin-correction、feat/misread-table-only；范围：全拼、双拼、混输的拼音侧、临时拼音
- 变化：
  - 整串打成表里的错误读音时，词库读不出的词也进候选：`tiaoyan` 前：无「调研」→ 后：「调研」排在
    完整候选末尾；`zhire` → 炽热、`bangbu` → 蚌埠。开「显示正确拼音」时内嵌
    `调(diao)研`，只标读错的字。
  - 召回候选权重取同页最低再低一档，存在完整的普通候选时不占首选，不计满码 / 短语自动上屏、
    不否决混输上屏；截断时与纠错候选共用保底名额。
  - 词库已收了错误读音的词不另召回，那一条按表标提示；简拼 / 前缀补全带出的同一个词不标
    （`yishn` 的「一刹那」不算打了 `yishana`）。
  - 选中召回候选后按**正确读音**学习（「调研」记在 `diao yan` 下，`learn_code` = 正确读音），
    不按用户打的 `tiaoyan` 造词，故它不会因为选过就变成 `tiaoyan` 下的普通候选。
  - 只认表，不做自动推断；错误拼写本身是高频词拼写的不收（`xinxin` = 信心，故不收「莘莘 xin xin」），
    打常用词时首页不混进召回项。评测集 A–F 类开 / 关误读逐位一致，打对的输入首页误读提示 0 条。
  - `[input.correction]` 任一项与两份误读表改动后，配置热重载即生效，不必重启。
- 触发条件：出厂即生效（`input.correction.hint.misread = true`）；内置表 `schemas/pinyin_misread.yaml`
  611 词（619 个错误读音），用户可在用户数据目录 `data/pinyin_misread.yaml` 追加或覆盖。
- 回退旧行为：`input.correction.hint.misread = false`。
- 钉住的测试：`wind-coordinator/tests/pinyin_misread.rs`、`wind-coordinator/tests/pinyin_correction.rs::builtin_misread_table_recalls_tiaoyan`、
  `wind-engine/src/pinyin/misread.rs`、
  `wind-candidate/src/candidate.rs::corrected_tests::misread_recall_does_not_take_top`
- 来源：内部（设计 `docs/design/pinyin-correction-and-hints.md` §6）

## 2026-10-23 · 拼音字母换位纠错：新增纠错候选、带标记、不占首选（0.128）

- 合入：feat/pinyin-correction；范围：全拼方案、混输的拼音侧、临时拼音；双拼不启用
- 变化：
  - 打错字母顺序或漏打 `g` 的输入出候选：`kiashi` → *开始、`yuoshi` → *优势、*有时…；
    `zhonguo` → *中国、`gonping` → *公平。前缀 `*` 是纠错标记，上屏文字不带。
  - 开 `gn_ng` 后另有：`gogn` 前：干呕功能、干呕… → 后：*共、*供…*工；`gognping` → *公平、
    `dagnshi` → *当时、`xign` → *型（「习惯」仍在）。
  - 纠错不删候选、不改其余候选的相对次序（切分、整句、简拼不走纠错边）：纠错候选按罚过的
    权重（与模糊同量纲）穿插其间，窗口内保底至多 4 条（取数上限的 1/10，不足 10 条的窗口不留）；原有候选可能因此被挤到窗口尾部之后
    （`yuo` 下「与」等 `yu` 子短语排到数百个 *有、*又… 之后）。
  - 打对的输入不变（`gongping`、`zhongguo`）；打字途中的半截（`zhon`）不带标记。
  - 存在完整的非纠错候选（主输入、临拼、快捷输入与特殊模式同一判据）（精确或模糊命中、吃满整串，简拼 / 前缀补全不算）时，纠错候选
    不占首选：把第一条这样的候选提到首位，其余次序不变。同一文字经纠错与非纠错都能得到时
    只留非纠错那条。
  - 混输满码自动上屏不把纠错候选计入「有拼音候选」：五笔 `hagn`（甗）照旧满码上屏。
- 触发条件：出厂即生效（`input.correction.enabled = true`，出厂开 `on_ong` / `vowel_swap`；
  `gn_ng` 出厂关——开它时 `xign` 这类简拼的首页会被 *型、*性… 占满）。
- 回退旧行为：`input.correction.enabled = false`；只去标记设 `input.correction.marker = ""`。
- 钉住的测试：`wind-coordinator/tests/pinyin_correction.rs`、`wind-engine/tests/pinyin_correction_floor.rs`、`wind-coordinator/tests/pinyin_correction_visibility.rs`、
  `wind-candidate/src/candidate.rs::corrected_tests`、`wind-engine/src/pinyin/hint_align.rs`
- 来源：内部（设计 `docs/design/pinyin-correction-and-hints.md`）

## 2026-10-23 · 拼音英文候选：位次、条数、首选让位、双拼默认位次（0.128）

- 合入：WindInput `676b4b70`；范围：拼音方案（全拼、双拼），空码补全页另含码表 / 混输
- 变化：
  - 全拼 `bus` 前：bus、不是 → 后：不是、bus；`rug` → 如果、rug；`but` → 不同、but。
    `test` / `app` / `old` / `web` / `hello` / `world` 英文仍第 1。
  - 小鹤双拼 `the` / `good` / `this` 前：英文第 1 → 后：中文第 1、英文第 2。
  - 开补全时 `world` 前：world、worldwide、worlds 占前 3 → 后：英文合计至多 2 条。
  - 空码补全页 `hi` 前：his、high、him、history、hi → 后：hi 第 1（码表 / 混输同样）。
- 触发条件：需开 `schema.pinyin.english_merge.enable`（出厂关）。首选让位只在首选是吃满整串、
  全为常用字、非拼凑句的拼音词，且（合并后 weight ≥ 3000 或为用户词）时发生；该英文完整单词
  在拼音或英文词频里选过（count > 0）即不再让。
- 回退旧行为：`exact_position = 1`（双拼回到英文可居首；首选让位无开关，选一次英文即免）；
  `max_count` 调大放宽条数。`prefix_complete` 已改名 `complete_min_length`（未发布过，无兼容）。
- 已知取舍：导入的大词库属用户词，不受 3000 门槛约束，其冷僻词同样会让英文排第 2。
- 钉住的测试：`wind-coordinator/tests/pinyin_english_policy.rs`、
  `wind-engine/tests/english_merge_pinyin_complete.rs`
- 来源：内部（续码表线）；参照 GH#229

## 2026-10-22 · 码表 / 混输英文候选：补全码长、位次、条数、两级否决上屏（0.128）

- 合入：WindInput `d67c5441`；范围：码表方案、混输方案
- 变化：
  - 码表新增 `complete_min_length`：设 4 时五笔 `gith`（中文有候选）也出 github；出厂 0 = 现状。
  - 码表开满码上屏时，英文词开头的 4 码（`abse` 荫棚 / absent）前：自动上屏 → 后：不上屏，
    等第 5 码出补全（修复：否决原先只看显示出来的完整单词）。
  - 混输开英文后：整串恰为英文单词（`good`）前：满码上屏 / 顶码 → 后：不上屏；
    码表与拼音都无候选时前：只出完整单词 → 后：至多 5 条前缀补全；截断时英文有保底席位。
- 触发条件：码表需开 `schema.codetable.english_merge.enable`；混输需开 `schema.mix.enable_english`（均出厂关）。
- 回退旧行为：码表 `block_commit = false` 且 `block_commit_on_word = false`；
  混输 `block_commit_on_english_word = false`；位次 `exact_position` / `complete_position`，条数 `max_count`。
- 钉住的测试：`wind-engine/tests/english_merge_policy.rs`、
  `wind-coordinator/tests/codetable_english_position.rs`、`wind-engine/tests/engine_manager.rs`（否决）
- 来源：维护者实测（短码不出英文）、论坛 t355

## 2026-10-30 · 码表行内通配：并查未启用扩展词库并标 `*`（0.129）

- 合入：WindInput（本提交）；范围：开启通配的码表方案、混输主码表
- 变化：行内通配（如 `uui?`）此前不看未启用的扩展词库，现在与反查模式一致（命令栏剪贴板反查 `dict.rev` 同样补查，码前带 `*`）：前追加这些命中
  （排在已启用之后，如「门头沟区」）。反查与行内通配里，未启用库命中的注释前加 `*`。
- 触发条件：需开 `input.reverse.lookup_disabled_dicts`（出厂关）；普通打字候选不受影响。
- 回退旧行为：关掉该开关。
- 钉住的测试：`wind-engine/tests/lookup_disabled_dicts.rs`、`wind-coordinator/tests/codetable_wildcard.rs`、
  `codetable_reverse.rs`
- 来源：维护者定（论坛 t348 / t361）

## 2026-10-30 · 混输下符号通配键首位作通配（0.129）

- 合入：WindInput `0c3caf8e`；范围：开启通配的五笔拼音混输方案
- 变化：通配键是符号（如 `?`、`/`、反引号）时，首位按它不再直接上屏标点，而是进缓冲作通配；
  字母通配键（`z`）首位仍按拼音。
- 触发条件：`schema.codetable.wildcard` 开且通配键为符号（出厂通配关、键为 `z`）。
- 回退旧行为：把通配键换回字母，或关闭通配。
- 钉住的测试：`wind-coordinator/tests/wildcard_symbol_lead.rs`
- 来源：维护者实测

## 2026-11-03 · 快捷输入历史并进英文段按热度重排（0.130）

- 合入：见提交 `feat(quick): 快捷输入历史并进英文段`；范围：快捷输入里同时启用英文与历史成员的实例
- 变化：文本透镜下，命中的历史（如 `font-size`）作为附加候选并入英文段，从第 9 位起算，与英文词
  同一道词频重排比热度（次数、最近一次），用得多的前移——历史用 3～4 次即可上到英文段首位，
  而用得更多的英文词（如 `font`）仍在其前。此前历史只落在英文段之后。
- 触发条件：成员含 `english` 与 `quick_input.history`、缓冲为纯小写前缀、词频开启；与词库同名的历史不并入。
- 回退旧行为：把历史成员排到英文之前（成员顺序优先），或关闭英文词频。
- 钉住的测试：`wind-coordinator/tests/quick_input_history.rs`（`hot_history_joins_english_segment_and_reaches_top`、`hotter_english_word_stays_above_history`）
- 来源：GH#253（维护者定方案 B）

## 2026-11-05 · 快捷输入历史在 Free / 大写英文透镜按英文调频排（0.130）

- 合入：见提交 `fix(quick): Free/大写英文透镜的历史按英文调频排`；范围：启用历史成员的快捷输入
- 变化：缓冲带符号或大写（`${R`、`Res`）时，历史候选此前只按累计次数排，不看英文调频策略；
  现在与文本透镜同一道英文词频重排（「一次到顶」下最近用过的居首）。与缓冲逐字相同的历史仍恒首位。
- 触发条件：`schema.english.frequency` 开启；策略为 `top` 时差异最明显。数字 / 词组透镜下的历史成员仍按次数降序，不过调频。
- 回退旧行为：关闭英文词频（回到次数降序、最近次之）。
- 钉住的测试：`wind-coordinator/tests/quick_input_history.rs`（`free_lens_history_follows_english_top_strategy`）
- 来源：用户反馈

## 2026-11-05 · 标点顶屏自动配对按右符号宽度回退光标

- 变化：候选随左符号顶屏时，此前按「候选前缀 + 左符号」长度回退，`你【】` 停在 `你|【】`；
  现在仅回退右符号宽度，停在 `你【|】`。TSF 将完整格数一次传入，避免 Shift 按住时多次调用覆盖。
- 触发条件：启用中文自动配对，并由标点顶出候选。
- 回退配置：无保留错误定位的开关；关闭 `input.auto_pair.chinese` 可停用中文自动配对。
- 钉住的测试：`handle_punct::pair_commit_tests::punct_top_commit_pair_cursor_ignores_candidate_prefix`
  （空前缀、单字、词组、扩展汉字，覆盖方括号与 Shift 圆括号）。
- 来源：Windows 记事本实测及 TSF 日志。

## 2026-11-05 · 加词编辑快捷键不再透传给宿主

- 变化：快捷加词面板内 Ctrl+Enter 打开编辑窗口后，此前 TSF 将回车再次交给宿主，可能导致聊天误发送；
  现在保留服务端的消费决定。面板内只返回 Consumed 的 Ctrl 组合也保持消费。
- 触发条件：Ctrl+= 进入快捷加词面板，再按 Ctrl+Enter；普通组码及临时模式的宿主快捷键仍清组合后透传。
- 回退配置：无保留穿透行为的开关；可用独立加词窗口快捷键 Ctrl+Shift+= 绕过面板。
- 钉住的测试：`shortcut_ownership_tests`、`wind_tsf/tests/ctrl_alt_cleanup_policy_test.cpp`，
  以及 `input_flow` 的 Ctrl 组合守卫、`quick_input_select_key` 的候选快捷键回归。
- 来源：论坛 t398、Windows 记事本 TSF 日志。
