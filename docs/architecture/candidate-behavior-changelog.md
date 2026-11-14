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

## 2026-11-13 · 拼音可固定到第 N 位；多条固定位置互不挤位（0.132）

- 合入：`b947264c..2eb0d989`；范围：全部方案的候选右键菜单与移动端长按菜单；shadow 读端（码表、拼音、混输、特殊模式、临拼、临英）
- 变化：
  - 拼音（全拼、双拼、混输拼音段）前：只能置顶，前移 / 后移灰 → 后：「置顶」之后多一项「固定到第 N 位 ▸ 第 1 位…第 K 位」（K = 每页条数，上限 20，不超过候选条数），前移 / 后移仍灰；码表同样有这一项。N 是**界面名次**：开简入繁出时变体占的格子也算，紧跟原字的变体格（被钉的词插不进去）与当前所在那格灰。移动端摊平成「固定到第 1 位」…「固定到第 9 位」，动作名 `pin_at_N`
  - 英文方案与临时英文**不给**「固定到第 N 位」（头部原文 / 大小写变形条在调位之后才插入，名次会偏），置顶照旧
  - 开了 emoji 扩展、候选里有 emoji 时「固定到第 N 位」整组灰（emoji 的挂靠随词移动，名次没法稳定折算），置顶 / 前移 / 后移不受影响
  - 同一编码钉了多条位置、且后钉的词原本排在先钉的词之前 前：先钉的被往前挤一格（`[x,B,y,A,z,w,v]` 钉 A→第 4 位、B→第 6 位，A 实落第 3 位）→ 后：每条都落在自己的位置上；只钉第 1 位时不受影响
  - 码表前移 / 后移到**自己也有固定规则**的邻居上 前：`[a,b,c,d]` 后移 a 得 `[b,a,c,d]`，再后移 b 碰巧换回（逐条插入的副作用；改两阶段后会变成 `[c,b,a,d]`）→ 后：显式与邻居交换，邻居的规则改写到本条原位，换回 `[a,b,c,d]`；前移对称
  - 开简入繁出时对单字之后的候选前移 / 后移 前：规则位置按含简繁变体（「出」后的「齣」）的列表写，读端不含变体，落点偏后 → 后：按读端列表写，落点正确；emoji 扩展候选同样不计位；单字母引导的重复上屏项不再给调位（置顶 / 前移 / 后移 / 固定到第 N 位都灰）
- 触发条件：出厂即生效
- 回退旧行为：无（设计变更 / 缺陷修复）
- 钉住的测试：`wind-coordinator/tests/candidate_pin_at.rs`；`wind-candidate/src/shadow.rs::tests::multiple_pins_do_not_shift_each_other` 等；`candidate_grid.rs::tests::reorder_positions_skip_items_inserted_after_shadow`、`reorder_and_pin_slots_under_filter_with_variants`、`pin_slots_skip_leading_repeat`；`wind-mobile` 的 `pin_at_capped_to_nine_on_mobile`；`tests/candidate_pin_at_emoji.rs`；`wind-store` 的 `move_shadow_swapping_matches_like_reader`
- 来源：GH#236 与跟帖（维护者定稿，设计稿 §5）

## 2026-11-13 · 右键删除类菜单改名，拼音 / 英文多字词加「在此编码下隐藏」（0.132）

- 合入：`d9957707..4940ff4f`；范围：全部方案的候选右键菜单与移动端长按菜单
- 变化：
  - 拼音 / 英文数据方案下 ≥2 字的词（系统词、用户词、临时词、整句、合成候选）前：「隐藏候选」/「删除用户词」/「删除临时词」→ 后：「屏蔽该词」，动作不变（按文字屏蔽）；其下新增「在此编码下隐藏」，只在当前输入码下隐藏，不删用户词、不按文字屏蔽（如 `nihao` 下隐藏「你好」，`nh` 下照出）
  - 拼音 / 英文单字、码表系统词、混输码表段、码表合成候选 前：「隐藏候选」→ 后：「在此编码下隐藏」，动作不变；emoji 扩展候选那项同样改名、仍灰
  - 短语、码表 / 混输用户词与临时词、group 成员、快捷输入历史与格式候选的文案不变；英文头部原文条不可删时两项都灰
- 触发条件：出厂即生效；删除热键仍触发「屏蔽该词」，「在此编码下隐藏」不绑热键
- 回退旧行为：无（设计变更）
- 钉住的测试：`wind-coordinator/tests/candidate_delete_menu.rs`；`handle_menu.rs::tests::candidate_delete_menu_labels`
- 来源：内部（维护者定稿，设计稿 §9）

## 2026-11-13 · 快捷输入历史记录纯小写原文（0.132）

- 合入：见提交 `fix(quick): 快捷输入历史记录纯小写原文`；范围：启用历史成员的快捷输入
- 变化：文本透镜下回车 / 选原文上屏的纯小写串（`kubectl`）此前不进历史，现在进，与带大写、带符号的一致；
  词库词仍不记。出厂位置（历史在英文之后）下并进英文段，不改拼音及其前成员的顺序；历史排到拼音之前时
  按成员顺序排前。
- 触发条件：成员含 `quick_input.history`。
- 回退旧行为：无（设计变更）；已记入的条目可在候选右键删除。
- 钉住的测试：`wind-coordinator/tests/quick_input_history.rs::text_lens_raw_commit_is_remembered_without_moving_pinyin`
- 来源：用户反馈

## 2026-11-13 · 命令输出、软键盘点击、配对插入的上屏收尾（0.132）

- 合入：1affbdce；范围：全部方案；命令栏 `type()` / 工具栏按钮 / `command:` 绑定 / mac·Linux 粘贴短语 / `ime.pair`、软键盘鼠标点击、自动配对
- 变化：
  - 自动配对「你（|）」后撤销上屏 前：计数按整段 2，靠延迟左移的透传位或迟到的光标回声碰巧刷成 1，时序没碰上就删掉「你（」→ 后：恒删光标前的「（」
  - 自定义成多字符左段的自动配对 前：光标按左段长度回退、落错位置 → 后：恒回退右符号 1 格
  - 命令输出 / 粘贴短语上屏后撤销 前：mac / Linux 按上一笔的长度删，Windows 删 1 个 → 后：删这次输出的整段（连续多段 `type()` 合为一笔；中间有 `key.tap` 等副作用时只删 1 个）
  - 码表自动造词 前：mac / Linux 上命令文本前后两字可能连成假词 → 后：命令输出打断滑窗
  - 软键盘鼠标点击 前：不补中英空格、不记统计、Windows 上断滑窗 → 后：补空格、记统计、喂滑窗，与物理键敲软键盘一致
  - 无占位联想（Excel / WPS 表格）中命令输出 / 软键盘点击 / 悬停提示上屏 前：Windows 靠光标回声收掉联想 → 后：出口显式收，不再依赖回声
  - 托盘 / 热键 / 按应用切方案时有未上屏编码 前：检索范围临时放宽带进新方案的下一次组码 → 后：随切换复原
- 触发条件：出厂即生效；滑窗相关需开码表自动造词（出厂关）；撤销需绑 `ime.undo_commit`（出厂未绑）
- 回退旧行为：无（设计变更）
- 钉住的测试：`wind-coordinator/src/session_exit.rs::tests::cursor_insert_counts_only_text_before_the_cursor`、`command_output_breaks_the_draft_while_softkeyboard_click_feeds_it`、`command_multi_segment_output_counts_as_one_undo`、`mode_switch_take_restores_relaxed_scope`；`handle_assoc.rs::tests::non_input_commits_exit_bare_assoc`
- 来源：内部（落点指引 §4.3；配对撤销由维护者真机复测发现时序依赖）

## 2026-11-13 · 鼠标点选 / CapsLock / 悬停提示上屏补齐收尾（0.132）

- 合入：12fd121d；范围：全部方案，非按键上屏来源（鼠标点选候选、CapsLock 钩子选词、悬停提示菜单上屏、移动端候选栏选词）
- 变化：
  - 检索范围临时放宽后用鼠标选字上屏 前：放宽带进下一次组合 → 后：随本次组合结束复原，与键盘选字一致
  - 鼠标选「你好」上屏后撤销上屏 前：只删 1 个字（光标回声把计数刷成 1）→ 后：删整词「你好」
  - 码表方案连续用鼠标选单字 前：不进自动造词滑窗 → 后：与键盘选字一样进滑窗；悬停提示上屏的文本不进滑窗、并打断滑窗
  - 配对符号栈的超时在这几种上屏时也刷新
  - 无占位联想（出厂开在 Excel / WPS 表格）下鼠标选词上屏 前：光标回声被当成用户挪光标，联想窗随即收起 → 后：联想窗留着，与键盘选词一致
  - 下一词学习 前：鼠标选的词与下一次上屏之间被光标回声断开 → 后：能连上，影响之后的联想排序
  - 以上都依赖光标回声在自提交宽限期（200ms）内到达；推送通路比按键多一程，真机上待核实
- 触发条件：打点 / 复原 / 保活出厂即生效；滑窗那一条需开码表自动造词（`schema.codetable.auto_phrase.enabled`，出厂关）；移动端选词不进滑窗
- 回退旧行为：无（设计变更）
- 钉住的测试：`wind-coordinator/src/session_exit.rs::tests::non_key_origins_mark_self_commit_and_restore_scope`、`draft_window_per_origin`；回声不退联想见 `handle_assoc.rs::bare_assoc_exits_when_user_moves_the_caret`
- 来源：内部（落点指引 §4.3 待定项定案）

## 2026-11-13 · 智能联想的下一词表改为后台加载（0.132）

- 合入：`eaddd84d`；范围：智能联想档（`input.association.kind = "smart"`）
- 变化：前：第一次出联想时在按键线程上同步读入下一词表，那一次上屏会顿一下 → 后：启动、配置生效时
  后台加载；表还没装好的那一两次联想不出「下一词」来源的候选，其余来源照出
- 触发条件：出厂即生效（桌面出厂不是智能档，不受影响）
- 回退旧行为：无（设计变更）
- 钉住的测试：`wind-coordinator` `handle_assoc::tests::bigram_table_loads_off_key_thread`、
  `data_needs::tests::smart_association_needs_bigram`
- 来源：内部（输入流程零读盘，feature-placement-guide.md §6）

## 2026-11-12 · 英文右键删词改为按文字屏蔽，混输拼音段与拼音方案共用屏蔽（0.132）

- 合入：`f88efe2e..7204dfa0`；范围：英文方案、临时英文（含快捷输入的英文成员，读同一份屏蔽）；混输（五笔拼音）的拼音段；
  拼音 / 码表 / 混输里混入的英文
- 变化：
  - 英文删 ≥2 字母的词：前：只在当前输入串下隐藏（`hel` 删 `help`，`he`、`help` 照出）→ 后：按词库原文屏蔽
    （`english` 桶 `code = *`），所有前缀、补全都不再出这个词库词。大小写不归一：删 `OK` 不连带 `ok`。
    输入原文那一条不受屏蔽：`raw_candidate = always` 照常钉首位；`in_dict` 档下被屏蔽的词仍算「词库里的词」，
    打全它（大小写跟随下打 `Help` 亦然）原文条照出，空格 / 回车上屏原文。单字母仍只按编码隐藏。
  - 英文用户词 / 临时词：删掉这段文字在所有编码下的条目、词频与联想，并屏蔽；自动造词不再把它学回来。
  - 头部原文候选（所打原文 / 大小写变形，无码）：前：删除写一条读端永远命中不了的规则 → 后：只有字面就是所打原文、
    且词库有字面相同的词时按那个词删；单独出现的原文条（`wxyzqk`；`usa`，词库只有 `USA`；屏蔽 `ok` 后的原文条 `ok`）
    与大小写变形条（`Us` 下的 `us` / `US`）删除灰掉、热键不执行，不再连带屏蔽同码的别的写法。
  - 临英里置顶 / 删除后候选窗当场按临英重建：前：按主输入路重建、读空缓冲，候选窗被清空 → 后：原文条与其余候选留着。
  - 码表 / 拼音系统词库里的模板词条（`$Y年$M月$D日`）删除：规则记模板源文本，读端认展开后的候选，删了当场不出、跨日有效。
  - 大小写跟随 / CapsLock 档下的置顶、删除、恢复默认：前：规则按显示形态（`Held`、`HELP`）落键、读端比词库原文，
    点了不生效、「恢复默认」恒灰 → 后：按词库原文落键，当场生效。
  - 拼音、码表、混输里混入的英文用户词 / 临时词：前：删除找错桶，一条都删不到 → 后：去 `english` 桶按编码真删。
    混入的英文系统词仍按宿主方案、宿主编码隐藏，不读英文的屏蔽集。
  - 混输里删拼音来源的多字词：前：写在混输自己的桶、只按当前编码隐藏 → 后：同拼音方案按文字屏蔽（写 `pinyin` 桶，
    用户词 / 临时词按拼音桶真删再屏蔽），混输与全拼 / 双拼都不再出；码表段出的同文词照出（`nihao` 删「你好」后
    `wqvb` 仍有「你好」）。拼音方案里屏蔽的词，混输拼音段同样不出，整句恰为它时换下一条覆盖整串的整句；
    混输拼音段分段上屏拼出被屏蔽的词不再学进拼音桶。码表段的删除仍按编码。
- 触发条件：出厂即生效
- 回退旧行为：无（设计变更 / 缺陷修复）
- 钉住的测试：`wind-coordinator` `tests/english_block_word.rs`、`tests/mixed_pinyin_block_word.rs`、
  `tests/template_system_word_delete.rs`；`wind-candidate` `shadow::tests::deletes_by_template_source`；
  `wind-store` `shadow::tests::english_block_rows_are_case_sensitive`；
  `wind-coordinator` `english_candidates::tests::in_dict_counts_blocked_word`
- 来源：内部（§8 第 11、12 条）

## 2026-11-12 · 拼音右键删多字词改为按文字屏蔽（0.132）

- 合入：`15f5955c..542a2923`；范围：全拼 / 双拼方案（拼音数据方案），含临时拼音、快捷输入的拼音段；码表不变；混输见上条
- 变化：
  - 删多字的系统词或整句：前：只在当前输入串下隐藏（`nihao` 删「你好」，`nh`、`ni'hao`、模糊音 `lihao` 照出）→
    后：屏蔽这个词，规则记为 `code = *`，所有输入串、所有来源（系统 / 用户 / 临时 / 整句 / 模糊音 / 简拼 / 联想）都不再出现。
  - 整句：被屏蔽的词不进整句图；整句恰好拼成被屏蔽的词时（`nihao` 由「你」「好」拼出「你好」）换下一条整句，
    首选仍覆盖整串、空格不只上半截。作为更长整句的一段不拦（`nihaoshijie` 仍出「你好世界」）。
  - 分段上屏拼出被屏蔽的词：上屏照常，不再自动学进临时词库。
  - 删多字的用户词 / 临时词（含与系统词同文的合并候选）：前：只删当前编码那条 → 后：删掉这段文字在所有编码下的用户词与
    临时词，连同词频与联想记录，并同样屏蔽这个词——选过的整句学进临时词库后删掉（如 `tazaijialikanshu` 的「他在家里看书」），
    不再出现，分段上屏拼出来也不再学回；同文的系统词也一并不出。
  - 单字、短语的删除不变（单字仍只在当前编码下隐藏）。
  - 单条显式加词（加词对话框 / 快捷加词 / 命令栏 `dict.add` / 设置页加词改词 / 单条转正）撤销同文字的屏蔽；
    导入词库、全部转正不撤，`hiddenByShadow` 计入被屏蔽的词。屏蔽行在设置页候选调整列表里以编码 `*` 列出，可删除恢复。
  - 拼音的用户分库编进系统词典，候选上分不出来源，删除时按系统词处理（只屏蔽，不改分库文件）。
  - 联想里被屏蔽的整词不出，在各联想源内先滤再截取，条数不变。
  - 单字的判定按可见字：带变体选择符、肤色、零宽连接的单个 emoji 与国旗算一个字，不按文字屏蔽。
- 触发条件：出厂即生效
- 回退旧行为：无（设计变更）
- 钉住的测试：`wind-coordinator` `tests/pinyin_block_word.rs`；`wind-engine` `pinyin::tests::blocked_word_*` /
  `sentence_equal_to_blocked_word_takes_next_best` / `blocked_sentence_with_grammar_still_has_replacement`；
  `wind-store` `shadow::tests::blockable_word_counts_visible_chars` / `star_is_plain_code_outside_pinyin`
- 来源：GH#269

## 2026-11-12 · 双拼、临时英文下右键删除系统词生效（0.132）

- 合入：5cf62ed8；范围：双拼方案主输入路；临时英文
- 变化：删除系统词的规则改写在读端同款的归一码上，此前写在击键码上、删了照样出现。
  - 双拼完整转换时：前：写在 `nihc` 上、读端取 `nihao` → 后：写在 `nihao` 上，当场隐藏，与全拼方案共享
    （全拼打 `nihao` 也隐藏，同置顶的既有口径）。
  - 临时英文缓冲带大写（Shift+H 进入）时：前：写在 `Hels` 上、读端取小写 `hels` → 后：写在 `hels` 上，
    删「Helsinki」这类词库里本身大写的词当场隐藏。词库小写、靠大小写跟随显示成大写的词（`Help`）删了仍出现，未改。
  - 短语、用户词、临时词的删除不变（仍按击键码）。
  - 旧版写在击键码上的规则从未生效，设置页规则列表里可能残留这类无效条目，可手动恢复。
- 触发条件：出厂即生效
- 回退旧行为：无（缺陷修复）
- 钉住的测试：`wind-coordinator` `tests/shadow_delete_readd.rs::shuangpin_delete_writes_normalized_code` /
  `shuangpin_readd_via_settings_shows_again` / `temp_english_delete_with_uppercase_buffer_takes_effect`
- 来源：GH#269

## 2026-11-12 · 删过的系统词再手动加回，候选重新出现（0.132）

- 合入：507ef005、317fce3a、380b8968；范围：全部方案类型（码表 / 全拼 / 双拼 / 混输）
- 变化：
  - 右键删掉一个系统词后，再手动加同码同文的词（加词对话框 / 快捷加词 / 命令栏 `dict.add`、设置页加词或改词、
    设置页单条转正），该码下这条候选重新出现，并撤掉那条删除规则。前：删除规则按文本过滤、不看来源，
    加回的用户词一并被滤掉，永远打不出。例：五笔 `aa` 删「戒」再加「戒 aa」；全拼 `nihao`（含 `ni'hao`）删「拟好」再加；
    全拼 `xv` 删「需」再加（词条码归一成 `xu`，也认）。
  - 只撤**同编码**上的删除：在简拼 `nh` 上删的词，加 `nihao` 后 `nh` 下仍隐藏。置顶规则不动。
  - 自动学词、临时词达阈值自动晋升**不撤**：删过的词不会因为又分步打出一次而复活。
  - 批量操作（导入词库、设置页「全部转正」）**不撤**：一批词里混着删过的，不悄悄复活。结果里多报一个数——
    导入的 `userWords` 段带 `hiddenByShadow`（入了库但被隐藏的条数），`temp.promoteAll` 的返回由纯数字改为
    `{ promoted, hiddenByShadow }`（设置端当前不读这个返回值）。设置端显示待接。
  - 混输方案的规则单独记账：已启用的混输方案里以该码表 / 拼音为成员的，规则一起撤（混输下删、切到纯码表再加也算）。
  - 双拼未完整转换时规则记在原始击键上（如 `jvq`），不按 ü 归一后的形态去撤。
- 触发条件：出厂即生效
- 回退旧行为：无（缺陷修复）
- 钉住的测试：`wind-coordinator` `tests/shadow_delete_readd.rs`（码表 / 全拼 / 分隔符 / ü 拼写 / 双拼 / 双拼未完整转换不撤 /
  混输 / 混输切码表加回 / 快捷加词 / 两种导入不撤且报数 / 自动学词不复活）；`wind-store`
  `shadow::tests::clear_deleted_for_words_only_touches_matching_deletes` / `clear_deleted_for_words_sees_hand_edited_file`；
  `wind-webdata` `tests::single_update_and_promote_clear_but_promote_all_only_counts`
- 来源：GH#308

## 2026-11-08 · Linux 皮肤穿透装饰改为整层合成，与 block 装饰交叉叠放时绘制顺序变化（0.132）

- 合入：9a0904c9（Linux 穿透装饰拆层）；范围：Linux（X11 / Wayland）候选窗，仅启用了带 `hit = "pass"` 装饰的皮肤主题
- 变化：
  - 穿透装饰先从主层拆出来算输入区，再整层合回：所有穿透装饰都在窗体下（z < 0）时垫在主层最下面，否则盖在
    主层最上面（连同候选文字与 z ≥ 100 的装饰）。前：所有装饰按 z 交错画进同一张位图。
  - 只有穿透装饰与 block / drag 装饰交叉叠放时看得出来：例如 block 装饰 z = 120、穿透装饰 z = 10 且两者重叠，
    前：block 在上 → 后：穿透装饰在上；两块都在窗体下、block 的 z 更大时同理反过来。
  - 与 Windows 本地窗口路径（穿透装饰在独立窗口里）的叠放规则一致；macOS 拆到子 panel，同一规则。
- 触发条件：出厂即生效（主题里有穿透装饰且与其它装饰重叠）
- 回退旧行为：无（设计变更：穿透装饰须单独成层，输入区才能不含它）；主题作者可调 z，让两类装饰不交叉
- 钉住的测试：`wind-ui` `manager_macos::tests::linux_skin_input_region_leaves_out_pass_decor`；
  `wind-scene` `candidate::host_regions::tests::premultiplied_over_and_under`
- 来源：内部（设计 theme-skin-animation.md §17.2）

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
