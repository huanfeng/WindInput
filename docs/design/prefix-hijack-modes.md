# 前缀夺取式模式

> 面向「要再加一个这样的模式」的人。当前实例：网址（`input.url`）、Unicode 码点
> （`input.unicode`）。z 夺取（`try_z_fallback`）复用同一套回退骨架，但入口不同。

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
| 中缀式（**尚无实例**） | 缓冲**中间**出现某字符 | 设想中的邮箱模式（`@`） |

判据：**触发信息在缓冲的哪个位置**。前缀式看的是整串相等，引导键式看的是单键 + 空缓冲。
两者的守卫条件不重叠，故可以共存；中缀式与前缀式会重叠（见 §5）。

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

## 5. 两个已规划的扩展

### 5.1 网址模式加词库候选

网址模式目前恒无候选（`enter_url_mode` 里 `candidates.clear()`）。要加「常用网址补全 +
最近使用优先」，**结构上已经就位**：`state.candidates` 与候选窗渲染都是现成的，
`handle_url_key` 的 `refresh` 闭包里补一次候选查询即可，按键分派一行不用动。

要定的只有两件事：候选从哪来（新词库 or 上屏历史 `recent_commits`），以及空格键的语义
——现在是「上屏缓冲原文」，有候选之后要改成「上屏高亮候选」，那会动到既有用户的肌肉记忆，
建议跟一个配置项而不是直接改。

### 5.2 邮箱模式（`@` 触发）

**注意它不是前缀夺取式**，照本文档抄会撞墙：触发点是「输入过程中出现 `@`」，
而 `try_prefix_hijack` 要求的是「缓冲 + 本键 == 某前缀**全等**」。`abc@` 里缓冲是 `abc`，
与任何前缀都不相等。

两条可能的路：

1. **让夺取闸门支持「后缀触发」**——在 `try_prefix_hijack` 里加一类「本键命中触发字符，
   缓冲整体作为已输入部分带进模式」的规则。代价是闸门从「全等比较」变成两种语义，
   且要想清楚 `@` 与标点流水线的仲裁（`@` 现在会出全角＠）。
2. **走引导键式**——把 `@` 登记进 `keys.key_actions`，但那条链的守卫是**空缓冲**
   （`try_activate_mode` 开头就要求 `input_buffer.is_empty()`），而邮箱恰恰要求缓冲非空。
   走这条得先放宽那个守卫，牵连面比路 1 大。

⇒ 倾向路 1。真做的时候先确认一件事：`@` 是 `Shift+2`，属 C++ 吃键真相表的
**Punctuation（含 Shift+数字）**，中文模式**无条件吃**，所以 C++ 侧不用改。

另外邮箱与网址的候选来源（常用后缀 + 自定义 + 最近使用）形态几乎一样，真要做建议
两者共用一个「补全候选源」，而不是各写一份——否则「最近使用优先」这类规则会分叉。
