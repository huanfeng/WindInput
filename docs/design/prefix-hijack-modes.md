# 前缀夺取式模式

> 面向「要再加一个这样的模式」的人。当前实例：网址（`input.url`）、Unicode 码点
> （`input.unicode`）、邮箱（`input.email`，**后缀触发**，见 §5.2）。
> z 夺取（`try_z_fallback`）复用同一套回退骨架，但入口不同。

## 1. 它是什么

**正常输入累积到某个前缀时，把已打的字符从主输入流里抢走，转进一个独占模式。**

```
用户按    u        +
        ┌────┐  ┌──────────────────────────┐
主流程   │码表│  │ try_prefix_hijack 命中   │
        │缓冲│  │ input_buffer "u" 被夺走  │
        │"u" │  │ → ModeKind::Unicode      │
        └────┘  └──────────────────────────┘
                            │ 退格退到 "u+" 边界再按一次
                            ↓
                 rewind_hijack 把 "u" 放回码表缓冲
```

与另外两类模式的区别：

| | 触发 | 例 |
|---|---|---|
| **前缀夺取式**（本文） | 缓冲 + 本键 == 某前缀 | `www.` / `u+` |
| 引导键式（`try_activate_mode`） | 空缓冲按下某个键 | `;` 快捷输入、`` ` `` 特殊模式 |
| 后缀触发（邮箱，§5.2） | 本键是某字符且缓冲非空 | `abc` + `@` |

判据：**触发信息在缓冲的哪个位置**。前缀式看的是整串相等，引导键式看的是单键 + 空缓冲，
后缀式看的是本键 + 缓冲非空。三者的守卫条件不重叠，故可以共存。

后缀式与前缀式在闸门里**共用同一个函数**（`try_prefix_hijack`），只是多一条分支；
它排在前缀分支之后，于是显式配置的前缀优先——用户把 `@` 写进 `input.url.prefixes`
时按他写的来，不去猜。

## 2. 骨架在哪

全部在 `crates/wind-coordinator/src/handle_url.rs`（历史原因住在 url 那个文件里，
文件头注释已点明）：

| 函数 | 职责 |
|---|---|
| `try_prefix_hijack` | 入口闸门。总开关全关时常数时间返回 |
| `active_hijack_buffer` | 「当前模式的缓冲是哪个字段」 |
| `can_rewind` | 缓冲是否退回到了夺取边界 |
| `rewind_hijack` | 撤销夺取，按 `RewindOrigin` 把快照放回来源流 |

`Rewind { snapshot, host_text, origin }` 与 `RewindOrigin` 定义在 `pipeline.rs`。

## 3. 加一个模式要碰的地方

分两类。**第一类编译器会替你找**（穷尽 `match`，加了变体不接就编译失败）：

| # | 位置 | 接什么 |
|---|---|---|
| 1 | `pipeline.rs` `ModeKind` | 新变体 |
| 2 | `debug_support.rs` `debug_active_mode` | 模式名字符串（测试断言用） |
| 3 | `handle_candidate.rs` `commit_by_offset` | 序号选词的行为（多半是 `return None`） |
| 4 | `handle_candidate.rs` `overlay_buf_edit` | `BufEdit`（缓冲 + 光标） |
| 5 | `handle_candidate.rs` `overlay_caret_parts` | caret 换算四要素 |
| 6 | `handle_mode.rs` `mode_badge`（1124 行那个 match） | 候选窗徽标全名 + 短名 |
| 7 | `layout.rs` `mode_layout_intent` | `candidate_layout` 配置项 |
| 8 | `comment.rs` | 注释模板覆盖两项 |
| 9 | `layout.rs` 测试里的 `cfg_with` 与 `MODES` | 两个都要动。只把模式加进 `MODES` 而忘了在 `cfg_with` 里给它的 `candidate_layout` 赋值，`every_mode_maps_intent_over_baseline` 会红在「Vertical 意图却得 false」上——那是 fixture 没配，不是实现错 |

**第二类编译器抓不到**，漏了不报错、只是行为静默错：

| # | 位置 | 漏了会怎样 |
|---|---|---|
| 10 | `handle_url.rs` `active_hijack_buffer` | 退格永远退不出去（`can_rewind` 恒 false） |
| 11 | `handle_url.rs` `rewind_hijack` 的退出 `match` | 落 `reset_exclusive_modes` 兜底：状态清得掉，但本模式的收尾不跑，留下半清理残局 |
| 12 | `coordinator.rs` `cancel_session` | Esc 落兜底，同上 |
| 13 | `handle_lifecycle.rs` `reset_exclusive_modes` | 那是**逐字段** `clear()` 的，漏了缓冲就在模式切换后残留 |
| 14 | `message_handler.rs` 的模式分派 `match` | 进得去出不来——按键无人处理 |
| 15 | `handle_url.rs` `try_prefix_hijack` | 模式根本没有入口 |

> ★ 10 与 11 是**成对**的：一边认得、另一边漏了，症状是「退格能退出，但退出后状态不对」，
> 且只在退到边界那一次出现。这两处的注释里各自写着对方的名字，改一处就去看另一处。
>
> ★ 13 与 11/12 看着重复，其实分工不同：11/12 走的是**本模式的 `exit_*`**（带自己的收尾），
> 13 是所有模式的**统一清场**（模式切换、方案切换那类路径进来的）。两边都要有。

配置侧另有两仓五道守门测试，见 `reference_wind_setting_repo` / 各测试自带的修复指引。

## 4. 未开启时的性能约束（硬要求）

`try_prefix_hijack` 在**逐键热路径**上，而这些模式出厂全关。约束：

- 总开关**在一次 `rt()` 借用里读完**，不是每个模式各借一次；
- 全关立即返回：不做键码转换、不构造探针 `String`、不遍历前缀表。

加模式时把新开关加进那个元组，不要另起一次 `self.rt()`——那正是「没开也变慢」的来源。

## 5. 两个扩展（均已实现）

### 5.1 网址模式的历史补全 ✅

已落地。候选来自**上屏历史**（`wind_store::completion` 的 `UrlHistory` 分区），不是新词库
——用户打的网址本就没有一份可预置的表。历史由 `input.url.history_enabled` 单独开关，
**出厂关闭**：网址模式本身不产生任何持久数据，开了历史才开始把打过的网址原文落盘，
两者隐私量级不同。

当年留的那道题（空格键语义）的答案：**空格与回车分工**，没有加配置项。

| 键 | 行为 |
|---|---|
| 空格 | 有候选 → 上屏高亮候选；无候选 → 上屏缓冲原文 |
| 回车 | **恒上屏缓冲原文** |

⚠️ 分工是实机反馈补出来的。一期把两个键并成一条路（`VK_SPACE | VK_RETURN => commit`），
于是**打了一半的邮箱再也上不了屏**——打 `abc@gm` 想就这么上屏，回车却给出
`abc@gmail.com`。回车在输入法里的通行语义就是「上屏我实际打的这串」，它是用户**否决
候选**的出口：候选越聪明，这个出口越不能堵。

不加配置项的理由：兼容性已被两条各自兜住——回车这一支与改动前逐字相同（那时恒无候选，
回车本就上屏原文），而空格那一支在出厂态（历史关闭 ⇒ 恒无候选）同样逐字相同。
只有主动开了历史的人才会遇到新语义，而那正是他要的东西。

实现见 `mode_completion.rs`（候选源与上屏收尾，与邮箱模式共用）。

### 5.2 邮箱模式（`@` 触发）✅

已落地，走的是当年倾向的**路 1**（让夺取闸门支持后缀触发）。实现见 `handle_email.rs`，
模式本体与 `handle_url.rs` 逐行平行。

拍板记录：

- **空缓冲按 `@` 不触发**。否则用户每次想单独打一个 `@` 都会掉进邮箱模式。这个条件
  同时也是「用户名从哪来」的答案——缓冲整体就是用户名。
- `@` 与标点流水线的仲裁不需要额外处理：`try_prefix_hijack` 本就排在标点臂**之前**，
  不触发时原样落回标点流水线。
- C++ 侧确认不用改：`@` 是 `Shift+2`，在 `ClassifyInputKey` 里归 `Punctuation`，中文模式
  无条件吃。本模式让 Rust 在**更多**情形下出字，是「C++ 吃键集 ⊆ Rust 出字集」的安全方向。
- 候选源确实**共用**了（`mode_completion.rs`），没有各写一份。当年那句担心是对的：
  排序、去重、上限、上屏收尾这四件事若分成两份，「最近使用优先」这类规则迟早分叉，
  而分叉的表现是「邮箱后缀按频次排了、网址历史没排」，两处代码看上去都对。

预置后缀表（`input.email.suffixes`）与学习数据**同域**：两边都存 `@` 之后的部分。
一边带 `@` 一边不带，去重就永远对不上，表现是常用后缀在候选里出现两次且消不掉。
