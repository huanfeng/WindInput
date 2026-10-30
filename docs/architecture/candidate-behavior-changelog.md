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
