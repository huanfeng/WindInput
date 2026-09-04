# 候选窗定位：宿主兼容性与必测矩阵

本文记录候选窗「画在哪」这一族问题的实测结论。2026-09 的一轮迭代里，同一批代码在微信、
QQNT、Excel、WPS 表格、EverEdit、记事本上暴露出**八种互不相同的错位形态**，每一种都只在特定宿主
的特定节奏下出现——**没有哪个宿主能代表其他宿主**。

> 首显档位（`fast`/`wait`/`instant`）本身的设计见
> [`docs/redesign/candidate-window-positioning.md`](../redesign/candidate-window-positioning.md) 第 6 层。
> 本文只讲宿主差异与测试方法。

## 一、核心模型：三个量必须分开

这一轮所有错位，追到底都是**同一个量被多个写入者争夺**。现在拆成三个量，各答一个问题，
任何两个合并回去都会立刻复发某一种错位：

| 量 | 答什么问题 | 谁写 | 何时失效 |
|---|---|---|---|
| `state.caret_x/y` | 宿主最近一次报的插入点（缓存） | `handle_caret_update`、`absorb_probe_coords` | 从不 |
| `shown_anchor` | 候选窗**此刻画在屏幕的哪里** | `notify_ui_update` 的首显与坐标校正 | `reset_first_show`（组合结束）；**不**含焦点切换 |
| `caret_baseline` | 上一次**被认可**的插入点 | `handle_caret_update` 的 settle 吸收与 reshow 两处 | `reset_first_show` |

四条不变量，各自对应一种已实测的缺陷：

- **缓存 ≠ 显示位置。** `probe` 和被 `settle` 吸收的坐标都会悄悄改写缓存，而候选窗并不跟着动。
  - 拿缓存当**校正基准** ⇒ 缓存跑到候选窗前面，之后任何比较都得出「没变化」，错位**永远无法
    自愈**（Excel 实测：候选窗留在编辑栏、真实位置在 1443px 外，而 `reshow` 判出 `dx=0`）。
  - 拿缓存当**重绘位置** ⇒ 悬停/翻页这类非坐标原因的重绘会把差额一次性补上（微信实测悬停跳
    13px，正好一个字符宽）。
- **显示位置 ≠ 认可基准。** `settle` 说的是「**这次**偏差不值得校正」，它没说「以后也不值得」。
  基准若跟着显示位置走，被吸收的偏差就永久留在基准里，下一帧再比又是同样的偏差，而 `settle`
  的放宽容差只在本轮第一帧有效（`swap` 消费掉）⇒ `tol` 掉回 3px ⇒ 必跳（微信「打第二三个字时
  候选窗自己挪一格」）。组合起点钉住位置时更严重：同一个 `dx` **反复**触发 reshow，实测连判
  十几次、每次走一遍完整下发。
- **`shown_anchor` 在焦点切换时刻意不清。** 它答的是「候选窗此刻画在屏幕哪里」，而焦点切换并
  不会把候选窗从屏幕上抹掉——清掉反而让校正判据失去唯一基准，错位再也纠正不回来。它只在候选
  窗真正消失时（`reset_first_show`）作废。
- **组合跨度 ≠ 起点错误。** 嵌入预编辑中，当前 caret 随组合增长而远离 `composition_start` 是正常
  现象。大偏移逃生阀若以这段距离判断「起点锁错」，长组合跨过阈值后就会把组合末端误锁成起点。
  但也不能因此全局信任非零 compStart——其它宿主有陈旧值和坐标系混用历史。QQNT 以 per-app
  `composition_start_pair_guard` 声明其特征，再由来源顺序、前帧降级点、reported compStart 与当前锁
  四重一致性识别帧对；未命中时仍走原 caret 大偏移自愈。

## 二、宿主分类与各自的坑

**按渲染/上下文行为分类，而不是按知名度。** 同一类里测一个即可，跨类必须都测。

### A 类：表格宿主——先在「旧上下文」建编辑上下文

代表：**Excel**、**WPS 表格（`et.exe`）**。

- Excel 进单元格时先在**编辑栏**建编辑上下文、约 0.5s 后才切到单元格。
- WPS 表格每个字都换一次 docMgr，且切换前先报**上一个单元格**的坐标与 `compStart`。

后果：`fast` 档的试探坐标（probe）抢在切换前把候选窗首显于旧位置，**组合起点也随之锁死在
那里**。此后即便校正判据判出 277px 偏移并触发 reshow，下发位置仍取那个锁死的组合起点——
表现为「候选窗恒慢一步」，而且日志里能看到 reshow 发生了却纹丝不动。

**出厂已配 `first_show_mode = "wait"`**（`data/compat.toml`）。`wait` 档整条忽略 probe
（日志「当前档位=wait 非 fast」），并由 `caret_pending` 握手把兜底延到 600ms，足以等过这次
切换，两个宿主实测均一次到位。

> ★ 元凶是 **probe** 不是 `caret_update`。排查时只顺着 `caret_update` 读代码会得出「`wait` 档
> 也会跳」的错误结论（本轮真实发生过，被用户实测推翻）。**首显有五条通路，改任何「要不要显示」
> 的判据都要逐条过。**

### B 类：Qt WebView——三项都不可信

代表：**微信（`Weixin.exe`，窗口类 `Qt51514QWindowIcon`）**。

| 不可信的东西 | 实测 | 应对 |
|---|---|---|
| `caret height` | 在 1 和 20px 之间跳变，`rect.bottom` 随之漂移 ~20px | `caret_use_top = true`，改用 `rect.top` 定位 |
| 组合期间的 `rect` | 停在**上一次组合**的位置，差 136~419px | `stale_probe_guard = true`，整条不收 probe |
| `compStart` | **恒等于当前光标 x**（逐帧 `compStart=(2241,783)` 与 `x=2241` 相等） | 由空闲上报坐标抢先锁定组合起点 |

第三项尤其隐蔽：这个宿主**根本没实现组合起点**。「同一组合只锁首个 compStart」那条守卫防的是
起点持续漂移，但它锁到的已经是**第一个字母落下之后**的位置——比真正的组合起点偏右一格。于是
首显用按键前坐标（对的），reshow 却改用宿主那份偏右的值，打第二个字母时候选窗自己挪 12px。

修法是让**组合前的空闲上报坐标**（按键前的光标位置，即真正的组合起点）抢在宿主之前锁定。

> ⚠ 判据用 `!shown`（本轮尚未下发过）而**不是** `is_first_frame`：兜底 timer 到期那条路径先置了
> `show_authorized`，`is_first_frame` 已为 false，绑在它上面会整条漏掉——症状是「第三个字好了、
> 第二个字仍挪一格」。

### C 类：纯文本编辑器——重排后才算得出坐标

代表：**记事本**、**EverEdit**。

不发或很少发 `OnLayoutChange`，组合期间的坐标要等文本重排完才算得出来。长按同一键
（typematic ~32ms/键，五笔满码 4 码自动上屏 ⇒ 组合寿命仅 ~128ms）时，一整段里可能**一条权威
`caret_update` 都不来**，缓存停在几百像素之外，每轮兜底都拿它首显 ⇒ 候选窗钉在原地。

应对：C++ 侧把重排前那一帧作为 `CARET_SRC_PRE_REFLOW` 发出来，服务端只拿它**刷新坐标缓存**、
不参与首显判据。刷新缓存不改变任何档位的首显时机，故这条处理放在档位门**之前**——`wait` 档
宿主同样需要（EverEdit 配的是 `wait`，曾因放在门后一条都收不到）。

### D 类：终端 / 浏览器

代表：**WindTerm**、**Windows Terminal**、**Edge**。

字宽大（WindTerm 实测 24px）、行内重排幅度大（同行左移 312px 实测过）。它们是**位置类启发式
判据的天然反例**——本轮四版「用位置判断这一帧准不准」的启发式，有两版就是被这类宿主推翻的。

### E 类：QQNT——同一按键交替上报起点降级帧与 selection 帧

代表：**QQNT（`QQ.exe`，窗口类 `Chrome_WidgetWin_1`）**。目前只在 QQ 复现，其他宿主未观察到
相同闪烁。

QQ 的 reported compStart 正常且稳定，但同一按键后会先后出现：

1. `TSF_COMPOSITION`：selection 暂时无效，DLL 用 composition start 降级成 caret；
2. `TSF_SELECTION`：selection 恢复，caret 回到组合末端，compStart 仍是原起点。

长拼音令组合末端与起点的距离超过 `3 × line_height` 后，若重锁判据看 caret 偏移，两帧会把锚点
反复改成 `start → selection → start`。`QQ.exe` 的出厂规则启用 `composition_start_pair_guard`；仅当
前帧确为 `TSF_COMPOSITION`、其 caret 等于本帧 reported compStart、且该点仍等于已锁起点时，
后续 `TSF_SELECTION` 才被判成正常组合跨度而禁止重锁。UI 的右边界钳制只放大了观感，不是根因。

## 三、必测矩阵

**每次改动候选窗定位相关代码，以下组合都要跑一遍。** 单个宿主全绿完全不能说明问题——本轮
八个缺陷里，没有任何一个能在两个以上宿主上同时观察到。

| # | 场景 | 必测宿主 | 看什么 |
|---|---|---|---|
| 1 | 切窗口/点击后打**第一个字** | Excel、EverEdit、微信 | 位置正确，不跳 |
| 2 | 连打 3~4 个字母（同一组合内） | 微信、记事本 | 候选窗**纹丝不动**（锚在组合起点） |
| 3 | 满码自动上屏后**立刻接着打**（五笔 `dddd` + `d`） | 微信、记事本 | 不抖；上屏改变插入点而缓存是上屏前的 |
| 4 | 鼠标悬停候选、翻页 | 微信 | 位置不动（非坐标原因的重绘） |
| 5 | 空格/换行/**退格**移动光标后再输入 | 微信 | 三种都要试，退格曾单独推翻一版判据 |
| 6 | 长按同一键不放 | 记事本、EverEdit | 候选窗跟着文字走，不钉在原地 |
| 7 | 极快速输入（脚本模拟 `d空格` 重复） | 记事本 | 候选窗仍出现（组合寿命可低至 19ms） |
| 8 | 进单元格第一个字 | Excel、WPS 表格 | 一次到位，不先落在编辑栏/旧单元格 |
| 9 | 长拼音持续输入，直到组合宽度超过 3 个行高 | QQNT | 候选窗始终锚在组合起点；不得在起点与末端间闪烁 |

场景 5 的三种操作**必须分别测**：本轮位置启发式被连续推翻四次，每次都是被一个新的操作方向
打掉的（字宽 → 换行 → 同行重排 → 退格），最后放弃位置判据、改成布尔判据才收敛。

## 四、日志判据速查

服务端日志（`%LOCALAPPDATA%\WindInputDev\logs\wind_input.log`，`level=debug`）里，一次首显必然
命中下面某一行，据此即可判定走了哪条路，不必对着 TSF 日志比时间戳：

```
first_show 闸门 → 立即显示（逃生口）: instant=? coords_ready=? idle_anchor=?
first_show 闸门 → 等待权威坐标（arm ?ms 兜底）: ...
first_show 兜底 timer 到期 → 用现有坐标首显（非权威，享放宽容差）
caret_update → 首显: 消费 pending_first_show，本帧作权威坐标
caret_probe → 提前首显: ...
```

排查错位时按这个顺序问：

1. **首显用了哪份坐标？** 看首显那行前面最近的 `记为组合前空闲上报 (x,y)` 或
   `caret_probe ... 已收入缓存`。
2. **随后位置变了吗？** `UpdateCandidates` 的 `pos=` 字段。同一组合内 `pos` 变化即「跳」。
3. **该校正却没校正？** 找 `caret_update → 忽略: 微移 dx=? dy=?`——若 `dx=0` 而候选窗明显错位，
   说明缓存被 probe 抢先刷新了，基准问错了对象（见第一节）。
4. **反复校正？** 同一个 `dx` 连续出现多条 `caret_update → reshow`，而 `pos` 不变——基准跟着显示
   位置走了。
5. **起点与末端对打？** 相邻日志在 `src=tsf_composition` 与 `src=tsf_selection` 间交替，reported
   `compStart` 不变，却出现 `组合起点重锁` 且 `UpdateCandidates pos=` 来回切换——把组合跨度误当成
   起点错误了。

一个统计口径的提醒：**连打时每打一个字光标本就前移一个字宽，随之而来的 reshow 是正确的跟随，
不是漂移。** 统计漂移率时必须只看「首显后、下一次按键前」的位置变化，否则会把正常跟随算成缺陷
（本轮首版分析脚本因此把漂移率报成 25.2%，实际 3.6%）。

## 五、已否定的方向（勿重试）

- **用位置关系判断「这一帧坐标准不准」**：连续四版被真机推翻——`.abs()` 抹平方向（WindTerm 字宽
  24px 误拦）→ 只判水平（换行时方向翻转）→「同行只前移」（终端同行左移 312px）→「前进就正常」
  （退格后陈旧值停在右边 390px）。最终改为布尔判据：某宿主的 probe 若恒不可信，就别去判**这一帧**
  准不准，直接改用另一个可信来源。**「判断做不出来」和「判断做错了」是两回事，前者只能换依据。**
- **给 Excel 这类宿主做「首显后撤回重来」**：会把「跳一下」换成「闪一下」，未必更好，且对所有宿主
  生效。已改用 per-app `wait` 档，零新增代码。
- **靠单个宿主验证**：见第三节。

## 六、相关文件

| 位置 | 作用 |
|---|---|
| `wind-coordinator/src/coordinator/first_show.rs` | 首显闸门、兜底 timer、`absorb_probe_coords` |
| `wind-coordinator/src/coordinator/message_handler.rs` | `handle_caret_update` / `handle_caret_probe` |
| `wind-coordinator/src/coordinator.rs` | `notify_ui_update` 里的位置计算与逃生口 |
| `wind-config/src/app_compat.rs` | per-app 规则（`first_show_mode`、`caret_use_top`、`stale_probe_guard`、`composition_start_pair_guard`） |
| `data/compat.toml` | 出厂 per-app 规则 |
| `wind_tsf/src/CaretEditSession.cpp` | selection 无效时用 composition start 作 caret 的降级 |
| `wind_tsf/src/TextService.cpp` | `OnAsyncCaretRectReady`、probe 发送与 caret source 标记 |

> ⚠ 用户层 `%APPDATA%\WindInputDev\compat.toml` 的合并语义通常是「同名进程**整条**覆盖系统层」。
> 排查「出厂规则不生效」时先看用户层有没有该进程的条目——菜单改回「跟随全局」曾会留下只剩
> `process` 的空壳条目，把出厂规则整条屏蔽掉（已修：写盘时剔除空壳）。
> `composition_start_pair_guard` 是协议级安全例外：用户层未写时继承出厂值，显式
> `false` 才关闭，因而已有的 QQ 稀疏自定义规则不会让本修复在升级后失效。
