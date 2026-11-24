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

同类还有 **Beyond Compare（`BCompare.exe`）**：组合期间的坐标与 compStart 恒为窗口左上角附近的固定值，
与真实位置无关。对 `stale_probe_guard` 宿主，组合期间不仅 probe 不收，普通 `caret_update` 也整条
不采信（缓存还原为组合前的空闲上报），否则首显对了、几十毫秒后第一帧 `caret_update` 又把候选窗
重锁回错位。没有空闲上报时本守卫不生效。

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

EverEdit 的空闲上报还会滞后一拍：点击移动光标后既不发 `caret_update` 也不发 selection 变更，缓存
停在上一次的位置，而 probe 在 1ms 后就给出正确坐标。因此 probe 可信的宿主不拿空闲上报抢跑首显
（`probe_is_untrusted` 为假时 idle_anchor 逃生口不开），否则候选窗恒慢一拍。

焦点刚到达时的空闲上报同样不可信：宿主还没 reflow，报的往往是上一处的位置（EverEdit 实测焦点坐标、
空闲上报、真实位置三者互不相同，拿空闲上报首显会错上千像素）。故焦点后的第一个组合不开 idle_anchor
逃生口（`awaiting_first_authority_after_focus`），仍走 25ms 短兜底，其间 probe 已把缓存刷对；
不能改为收窄 `caret_cache_verified` 的置位，那会让焦点后首字退回 600ms 长兜底。

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

QQ 的另外两个事实，决定了重锁判据的写法：

- **判据问宿主，不问自己**：「起点没错」的依据是宿主前后两帧报的 compStart 一致，而不是拿已锁的
  `composition_start` 与它比。后者有三个写入者，其中两个写的是「候选窗画在哪」，与宿主值差 2px
  就会让相等判据整条失效。
- **输入框整体移动时，QQ 会如实改报 compStart**（x 不变、y 随框移动）。此时前后两帧不同、要重锁，
  而对开了 `composition_start_pair_guard` 的宿主，重锁目标取宿主本帧报的 compStart；取 caret
  会把锚点锁到组合末端，之后 guard 反而把这个错锚点钉住。
- 组合区换行后 QQ 会补发一帧 `TSF_CACHED`，带着第一行行尾的陈旧坐标。只认
  `TSF_COMPOSITION → TSF_SELECTION` 帧对的判据管不到它。

### F 类：游戏宿主——`GetTextExt` 返回**固定**垃圾值

代表：**流放之路（`PathOfExile.exe`）**；论坛另有多款游戏反馈相同症状（候选窗钉在屏幕左上角）。

★ 它**不接管 UI**（`uielement state reported: flags=0x0 (host_draws=0 ui_less=0)`、
`BeginUIElement ok id=0 show=1`），候选窗是我们自己画的，所以这条**是我们能修的**——与 Dota 2
那类「宿主自绘候选、数据侧调不动」是**完全不同**的两件事，别套那边的结论
（见 `../design/game-compat-tsf-uielement.md`）。

2026-09-12 真机日志（`wind_tsf.PathOfExile.47636.log`）里两个坐标**整场一个像素都没变过**：

| 量 | 值 | 判定 |
|---|---|---|
| selection 的 caret rect | `(3839, 2063, 3840, 2063)` | 屏幕右下角、**高度 0** ⇒ 判无效 |
| composition start rect | `(13, 9, 13, 44)` | 屏幕左上角、高度 35 ⇒ **被降级采信** |
| `GetScreenExt` | `(273, 216, 2833, 1656)` | 游戏窗口真实范围，**合理** |

于是锚点降级（caret 无效 → 用组合起点，见 `CaretEditSession.cpp` 的「★ 锚点降级」）采信了
`(13,44)`，候选窗就钉死在屏幕左上角。两个值都是常量 ⇒ 该宿主根本没真正实现 `GetTextExt`，
**真实光标位置无从获得**；但「识别出这是垃圾」可行：

★ **判据：组合起点落在 `GetScreenExt` 之外**——`(13,44)` 不在 `(273,216)-(2833,1656)` 内。
这正是 `CaretEditSession.cpp` 降级分支里那句「若实测可靠，将来可取代『所有显示器』做越界校验」
所等的证据：本例中 `GetScreenExt` 可靠（同一场里还出现过 `(0,0,3840,2160)`，会随状态变化，
不是死值）。

⚠️ 但**别无条件信它**：`GetScreenExt` 在 shell context 上实测返回过退化矩形 `(0,1368,0,1368)`
（同文件另一段注释），校验前先确认它自身非退化。

⛔ **流放之路本身无解，别再为它投入**：同一宿主上**搜狗与微软拼音的候选窗也在左上角**
（2026-09-12 用户实测）——它没有通过任何通道给出有效位置。本类的价值在于**其它**游戏：
论坛有多款游戏反馈同类症状，其中若有走 IMM32 通道的，就能救。

### ★ 第三条通道：IMM32 的 `CANDIDATEFORM`（已加探测，待实测数据）

游戏类宿主往往**根本没打算**用 TSF 传坐标：SDL 的 `SDL_SetTextInputRect` 在 Windows 上就实现为
`ImmSetCandidateWindow`（写 `CANDIDATEFORM`），多数自绘 UI 的游戏同理，而这条路**不会**反映到
`GetTextExt` 上。我们此前只听 TSF 一条通道，这是缺口。

`CaretEditSession.cpp` 的降级分支已加 **只记日志、不改行为** 的探测（`ImmGetCandidateWindow` /
`ImmGetCompositionWindow`，同时打客户区与 `ClientToScreen` 后的屏幕坐标）：

```
CaretEditSession: IMM32 probe hwnd=0x... cand=1 style=0x... client=(x,y) screen=(x,y) area=(...) | comp=...
```

`cand=1` 且坐标合理 ⇒ 该宿主有救，可把这条接进降级链；`cand=0` 或 `无 IMC` ⇒ 与流放之路同类，无解。
⚠ 注意 `ptCurrentPos` 是**客户区**坐标，比对时别和屏幕坐标混参照系。

① **已实施**（2026-09-13）：降级采信坐标前做越界校验，不在 `GetScreenExt` 内即不采信，
让 caret 保持无效——流放之路的 `(13,44)` 与 `(3839,2063)` 都由它挡住。实现与失败关闭的
理由见 §G 类。

**仍未实施**的方向，均需真机验证：
② 把上述 IMM32 坐标接进降级链（取决于探测数据，目前只有流放之路一个样本且它没设）；
③ 都拿不到时用窗口内兜底锚点（游戏聊天框多在下方，底部中央远比左上角合理），
这需要把 `GetScreenExt` 经 IPC 传到服务端。

### G 类：只对**非零长度** range 给矩形，且不填高度（**有解，已修**）

代表：**洛克王国（虚幻引擎，`NRC-Win64-Shipping.exe`）**。

⚠️ 与 F 类症状相似但**根子完全不同，别混**：F 类给的是**固定垃圾值**（整场不变），无解；
G 类给的是**正确但残缺**的矩形，能修，而且已经修了。分辨方法：看坐标随输入**变不变**。

2026-09-13 真机日志（`wind_tsf.NRC-Win64-Shipping.*.log`）的规律：

```
GetTextExt failed hr=0x80040505                    ← selection（零长度）→ TS_E_NOLAYOUT
Composition start GetTextExt failed hr=0x80040505  ← 组合起点（零长度）→ 同样失败
Composition rect (1353,1647,1399,1647) w=46 h=0    ← 组合整体（非零长度）→ 成功，但 bottom==top
```

**对零长度 range 一律回 `TS_E_NOLAYOUT`，只有非零长度 range 才给矩形，且只填水平信息。**
宽度随编码串增长（16→19→25→28→46）、`left/top` 随输入移动 ⇒ 宿主**已经算完布局**，只是不填
高度——这与「退化矩形 = 布局没算完」那个前提**正相反**，不该一并丢弃。

修前三条路全断，退到兜底坐标 `(640,332)`，而真实位置 `(1353,1647)` 就摆在同一帧日志里没被用。

**已实施**（`CaretEditSession.cpp` 的「★★ 二级降级」）：caret 与组合起点都拿不到、但组合整体
矩形有效（宽>0）时，用它的左上角当 caret，高度依次由两个来源决定：

1. `WIND_DEFAULT_CARET_HEIGHT` 按**宿主视角**的 DPI 换算（`GetDpiForWindow`）。⚠ 必须取宿主
   视角而非主屏真值：`GetTextExt` 的坐标就在宿主的感知级别下，宿主 unaware 时它回 96、坐标也是
   虚拟化的 96dpi，正好不该缩放；套用 `LangBarItemButton` 抬高线程感知级别那套反而错。
   实测该宿主 200% 缩放 ⇒ 40px，而补 20 个设备像素只有真实行高一半，候选窗会压住正在输入的那行。
2. 再与 `GetScreenExt` 的 `bottom` 取 **min**——宿主声明的显示区下沿才是这一行真正的底。
   实测 `top=2004`、换算高度 40 ⇒ 2044，而显示区 `(1066,1948,2174,2008)` 的下沿是 2008，
   取 min 后候选窗贴合输入框（否则低 36px，肉眼可见偏下）。

⚠️ 二级降级**失败关闭**（`GetScreenExt` 拿不到就跳过），与一级降级的放行相反。两边代价不
对称：跳过只是退回没有本级时的行为（无损）；放行则可能让 F 类那份垃圾坐标以
`CARET_SRC_TSF_COMPOSITION` 的名义通过下游每一道闸，把"没拿到坐标"伪装成"拿到了权威坐标"。

⚠️ 二级降级**不支持顶码偏移**（`_compStartOffset`）：整体矩形无法按 wchar 切分出余码段。
已知取舍——这类宿主连组合起点都给不出，没有更精确的来源。

### H 类：组合矩形是真范围，但 `left` 随组合变长而右移（**有解，已修**）

代表：**Tabby（Electron 终端，组合层是 xterm.js）**。

⚠️ 与 B 类第三项（微信「根本没实现组合起点」）**不是一回事**：那家的 `compStart` 恒等于当前
caret，这家给的是**货真价实的范围矩形**（`w` 从 14 长到 39），只是 `left` 也跟着动。判据一
（`w > INSERTION_POINT_MAX_WIDTH`）对它全程放行，所以 `pin_anchor_when_start_drifts`
——它的前提是宿主给**插入点形态**矩形——在这家身上**天然不适用**，开了也没用（实测开关
已生效、行为零变化）。

2026-09-17 靶机 192.168.5.30，一次组合 `w → wv → wvs → wvsr` 的原始帧：

| buf | compRect | w | left |
|---|---|---|---|
| `w` | (379,1174,393,1205) | 14 | 379 |
| `wv` | (381,1174,405,1205) | 24 | **381** |
| `wvs` | (384,1174,417,1205) | 33 | **384** |
| `wvsr` | (390,1174,429,1205) | 39 | **390** |

组合内容只增不减 ⇒ 起点按定义不动，这家四码把它推了 11px。候选窗锚在组合起点上，于是
**每打一码挪一下**，用户报「输入到 2/3 码时抖动，几乎百分百」。同一份日志里 Tabby 161 次
同组合内重锁，而下面的对照组 0 次。

★ **对照组决定了修法能不能做成全局**——同一份日志里：

| 宿主 | 组合从 w=14 长到 w=48 期间的 `left` | 同组合内重锁 |
|---|---|---|
| Chrome | 恒为 918 | 0 |
| VS Code | 同上（同一套 Chromium TSFTextStore） | 0 |
| 记事本 | 恒定 | 0（27 次首锁） |
| **Tabby** | 379 → 381 → 384 → 390 | **161** |

⇒ **不是 Chromium 族共性，是这一家自己的缺陷**。Chrome 与 VS Code 用的就是同一套
`TSFTextStore`，`left` 纹丝不动。所以判据做成全局默认而**没有**加 per-app 开关：对 `left`
稳定的宿主它是 no-op（`ax` 本就等于 `cs.0`），只有报错的宿主会被挡住。这与
`pin_anchor_when_start_drifts` 的处境相反——那条上 WPS 文字与 Excel 的数据完全相同而期望
相反，全局判据在数学上不存在（见该字段注释与 2026-09-05 的翻车记录）。

**已实施**（`message_handler.rs` 的 `rect_anchor` 采信处）：同一组合、同一行里矩形只给**一次**
锚点，之后的 `left` 漂移不采信。

- ⚠ **跨行放行**（`ay != cs.1`）：「矩形自动落到最后一行行首」正是这条路径存在的理由。
- ⚠ 锁用**独立**的 `rect_anchor_locked`，不是 `composition_start.2`：后者是路径 A（DLL 上报
  `compStart`）的锁，而新组合第一帧的 `compStart` 常是陈旧值——本机实测路径 A 锁进上一组合
  末端 `453`，靠矩形修正成真起点 `442`。绑在 `cs.2` 上会把这次**必要的修正**一起挡掉，
  错位 11px 且不自愈。两条路各锁各的，清位也要各清各的（组合结束 + 焦点换 docMgr 两处）。

> 这是「**挡住一处不等于挡住了**」的第三例（前两例：`caret_cache_verified` 的两个入口见
> `144925d8`；行尾归一化的多入口见 `e4cacf64`）。`composition_start` 有两条写入路径，路径 A
> 的注释写着「防部分控件 GetRange 让起点随输入漂移，致候选窗随输入右移」——正是本症状——
> 而漂移从没有守卫的路径 B 长驱直入。**给一个状态加守卫时，先数它有几个写入口。**

### I 类：Typora 在段尾输入时，候选窗跳到左上角

在 Typora 0.9.98 中，用一段长文字撑出两行，把光标放在第二行末尾，候选窗会在打第一个字符时在正常位置，
打第二个字符时从光标附近跳到窗口左上角。在同一行靠前的位置输入则正常。

清风定位候选窗时，会向应用查询光标位置和当前编辑区的边界。边界用来过滤还停在上一个输入框的
旧坐标。这次 Typora 报的光标位置是对的，报出的右边界却比光标还靠左 12 像素。清风因此丢掉了
正确的光标位置，改用 Windows 提供的备用位置，恰好落在窗口左上角附近。

更新候选窗位置时，如果清风因为坐标超出编辑区而拒绝使用它，就跳过这次位置更新。已经显示的候选窗
会留在原地，等应用给出可用的坐标再更新。光标是否越界仍按原来的规则判断。

复测时，可以先在同一行靠前的位置组字，再把光标移到段尾，重新输入 `c`、`e`、`u`。
这样可以分别观察第一键能否在正确的位置显示，以及后续按键会不会把窗口挪走。
实现见 [TSF 光标查询](../../wind_tsf/src/CaretEditSession.cpp)和[候选窗位置更新](../../wind_tsf/src/TextService.cpp)。

## 二·五、垂直可用区：全屏时不该扣任务栏

候选窗「下方放不下就上翻」的判据（`candidate_window.rs` 的 `place_window`）原本用 `rcWork`
（排除任务栏），而候选窗真正的钳制函数 `clamp_content_to_monitor` 用的是 `rcMonitor` 并注明
「允许摆到任务栏上方」——**判定比钳制严格**，于是判「放不下」的窗口实际本来放得下。

全屏游戏下任务栏根本不可见，这条白扣得尤其冤。洛克王国实测：caret 底端 `y=2024` 时

| 判据 | 下方可用 |
|---|---|
| `rcMonitor`（屏幕底 2160） | 134px |
| `rcWork`（扣任务栏约 48px） | 86px |

高度落在 86–134px 之间的候选窗被误判上翻，反过来遮住正在输入的那一行。

**已实施**：按「前台窗口是否铺满 **caret 所在那块屏**」在 `rcMonitor` / `rcWork` 之间选。
桌面行为一字未变，只有全屏那一种情形多出任务栏那条高度。

★ 判据收口在 `wind-keys::foreground::foreground_covers_monitor`，**不要在别处重写**：几何比较
只是它的一半，后面两道守卫才是它能用的原因，而那两道都是实测命中后补上的——

- **DWM cloaked**：实测命中 ClickToDo 的 `IslandWindow`、**TextInputHost 的 `CoreWindow`**
  （shell 输入宿主，与输入法场景高度相关）；
- **属 explorer 进程**：实测命中 `XamlExplorerHostIslandWindow`（Win11 开始菜单/任务视图/搜索），
  它 rect 精确等于显示器且**不是** cloaked，前一道拦不住。

这两类窗口「焦点切换的一两毫秒中间态里可能短暂成为前台」，而 `place_window` 恰好在候选窗要显示
的那一刻跑，正落在这个窗口期。漏掉守卫的后果：桌面上光标贴近屏幕底部时候选窗压住任务栏。

⚠️ **不能**改用 `foreground_fullscreen_kind() == Covering`：那个用 `MonitorFromWindow(前台窗口)`，
答的是「前台窗口自己那块屏」；这里必须问 caret 所在那块屏——多屏下 A 屏的全屏游戏不会隐藏
B 屏的任务栏。

⛔ 也**不要**改用 `SHQueryUserNotificationState` 判任务栏可见性：它对同一个 Dota `SDL_app` 窗口
逐键抖动（一会儿 `D3dExclusive`、一会儿 `Covering`），会让候选窗在上下方之间反复跳。
（判 D3D 独占态仍是它的正当用途，`foreground.rs` 判据①靠 300ms TTL 压住抖动。）

## 二·六、组合矩形（`composition_rect`）的三种形态

组合矩形是宿主对整个组合范围的测量。它可信时直接取左下角作锚点：单行时 `left` 是组合起点、
`bottom` 是行底；跨行时 `left` 落在最后一行行首。compStart 做不到这一点：它把组合折叠到起点，
跨行后仍停在第一行（QQ、飞书、记事本实测均如此，三者的 `caret.y` 恒等于 `rect.bottom`），这是
候选窗「换行后回不去」的根源。同行内 `left` 恒定，所以矩形每帧更新，不受「同一组合只锁一次」约束
（例外是 H 类 Tabby，见上）。

可信要同时满足两条（`handle_caret_update` 的 `rect_anchor`）：

1. **是范围不是点**：宽度 > `INSERTION_POINT_MAX_WIDTH`（4px）。光标宽 0~2px，真包围盒最窄也是
   一个字符宽（16~17px），中间是空带。
2. **覆盖当前插入行**：`caret.y <= rect.bottom`。只返回首行的宿主会在换行后违反它，此时回退既有逻辑。

不可信的两种形态，处理不同：

| 形态 | 代表 | 含义 | 处理 |
|---|---|---|---|
| **整帧退化**（宽或高为 0） | Word 的 `tsf_cached` 帧 | 这次 `GetTextExt` 没算出布局，同帧的 caret 与 compStart 也不可信 | 抑制大偏移逃生阀，否则它与同期 selection 帧（差 100+px）交替到达，锚点来回抖 |
| **插入点形态**（宽 1~2px） | WPS 文字（w=2）、WPS 表格 / Excel（w=1） | 宿主给不出组合范围，compStart 跟着插入点右移 | 见下 |

插入点形态的宿主分两种，**数据完全一样、期望相反**，只能按宿主声明：

- **WPS 文字**要钉住起点。若放任大偏移逃生阀，打字时锚点被漂移的 compStart 一路推远，删除时
  逐字回退每步只有十几像素，永远够不到 3 倍行高的重锁阈值，锚点永久卡住。故 per-app
  `pin_anchor_when_start_drifts` 同时抑制逃生阀；跨行时 x 保持钉住、y 跟随 caret 所在行。
- **Excel / WPS 表格**每输入一个字就换一次 docMgr，候选窗本就该跟着单元格走，靠的正是逃生阀。
  `pin_anchor_when_start_drifts` 曾做成全局默认，当场弄坏 Excel 的跟随。

大偏移逃生阀（caret 偏离已锁起点超过 3 倍行高才重锁）有已知盲区：偏差落在 `tol_x`（约 2 倍行高）
与 3 倍行高之间时 reshow 会触发、位置却仍取钉住的起点。阈值不能再往下调（会把逐字前移当成锚点
错误），真遇到这个区间要换证据。有可信矩形时逃生阀整段跳过。

## 二·七、非 TSF 坐标来源

- **`gui_caret`**（`GetGUIThreadInfo`）可能是任务栏等别处残留的 Win32 光标。它够格当「没有更好
  选择时的兜底位置」，不够格解除首帧信任门（`caret_cache_verified`）。它与 TSF 的 compStart 不同源，
  锁组合起点时的 500px 同源校验对它不适用：此时直接采信 compStart。
- **Word 组合期的 `gui_caret` 帧**：Word 在按键期可拒绝同步 edit session（`TS_E_SYNCHRONOUS`），
  DLL 回退到 Win32 光标上报，那是跨窗口的陈旧位置（恒在别的行），不带组合起点与矩形。让它进入
  组合期的跟行 / 重锁，候选窗就在它与 TSF 真坐标之间来回跳。故本组合收到过 TSF 域帧之后，GUI 帧
  一律不再采信（`tsf_frame_seen_in_composition`）；纯 GUI 回退的宿主不受影响。
- **焦点事件上的坐标**：`OnSetFocus` 不是按键上下文，同步 edit session 必被拒绝，回退链交出的是
  跨窗口的 Win32 光标（Word 只在正文行维护它，标题行上取到的是别处的值，实测偏差 800px 以上）。
  `ui.status.show_on_focus` 的焦点气泡因此在 `follow_caret` 模式下只认 TSF 域坐标，拿不到就挂起
  等 DLL 补来的 TSF 帧（`pending_focus_tip`），不做回退。
- **Java 被动客户端**（如 JTable 未进入编辑）按键时 DLL 退到 `gui_caret`，拿到的是窗口客户区原点；
  TSF probe 报的是 Windows 输入编辑框的位置，它不随单元格移动，常恰好等于上一轮权威坐标而被判
  「未 reflow」。此时 probe 仍应替换掉非 TSF 的缓存，否则兜底到期时候选窗先闪到屏幕左边。
- **`tsf_default_pos`**：宿主不知道插入点在哪（如 Illustrator 画布），DLL 报宿主的默认位置。缓存装着
  它时不算「对应当前插入点」，`caret_cache_verified` 必须清为 false，否则下一次组合会走 25ms 短兜底，
  先在上一次 `gui_caret` 位置闪一下再跳到默认位置。代价是该宿主恒 arm 600ms 长兜底：默认位置帧
  通常几十毫秒内就到并消费掉兜底，但 `IsHostDefaultPosition` 失败关闭时会等满 600ms。
  - 识别指纹：`GetTextExt` 返回高度为 0 的退化矩形，选区、组合起点、组合矩形三者同值，且重试窗口
    过完仍是它（Illustrator 画布恒为工作区右下角最后一个像素；多个宿主给同一个值，是 TSF 的通用
    惯用语而非脏数据）。
  - 要采信、不能丢：其它输入法直接采信，候选窗落在右下角任务栏之上；丢掉它回退链会跌到 `gui_caret`
    （无关控件的 Win32 光标），候选窗被钉在左上角。采信后 `place_window` 现有的上翻 / 左移 / 钳制
    就会把它落到右下角，无需新代码。
  - 它不属 TSF 语义域（`caret_source::is_tsf` 为 false），因为它明说「没有插入点」，不能当权威坐标
    参与跟行与漂移校正；但照常走首显（`pending_first_show` 消费不看来源），不再空等 600ms。
  - 跟行（`pin_anchor_when_start_drifts` 把锚点 y 换成 `caret.y`）不看来源，本值也会被用上。没补来源闸，
    因为该指纹下 `caret.y` 恒等于已锁锚点的 y，构造不出可达的失败。
- **`imm_candidate_form`**：宿主经 `ImmSetCandidateWindow` 声明的候选窗位置（DLL 已换成屏幕坐标）。
  DLL 只在出现上面 `tsf_default_pos` 指纹时才取。典型宿主是走 CUAS 的 Java AWT/Swing（医疗 HIS/LIS
  客户端）：`GetTextExt` 恒为工作区右下角，而它把组合起点下沿写进 `CANDIDATEFORM`，微软拼音能跟随
  光标靠的也是它。它属 TSF 语义域（点就是组合起点下沿）。这类帧不带组合起点：宿主写 `CANDIDATEFORM`
  是异步的，组合首帧多是上次组合的旧值，带上会被锁成锚点、整场偏一截；不带则锚点跟逐帧 caret 走。

## 二·八、热键直达模式的占位 composition（`input.caret`，`CaretPlacementConfig`）

加词、临拼、特殊模式、生僻字等由热键激活的模式，激活瞬间没有输入内容却要立刻显示候选窗 / 预览窗。
做法是发一个占位 composition：C++ 侧 `StartComposition` 并 `SetText(" ")` 撑开 range，再
`GetTextExt` 取矩形。很多应用的 `GetTextExt` 之外的 Win32 取坐标 API 给不出正确坐标，只有基于
composition 的才准，所以不能一律关掉。

代价是占位 composition 建在当前 selection 上（`StartComposition` 不折叠、不改写 `tfSelection.range`），
用户选中着文字时那个空格会把选中内容替换掉。这是 TSF 标准语义，Qt / Electron / QQ 都会发生。

- 开关为 `false` 时协调器返回 `Consumed`、完全不发 `UpdateComposition`，DLL 不建 composition，选中内容
  保留；坐标退到 `GetCaretPosition` 的回退链（GUITHREADINFO → GetCaretPos → 上次已知位置 → 窗口中心估计）。
- 已否定：建了 composition 再折叠成插入点。只要对 composition 做操作，选中内容就会被清空，且与宿主有关。
- 开关为 `false` 时后续 ↑↓ / Enter / Esc 的转发靠 C++ 的独立位 `_hotkeyModeSession` 撑起输入会话
  （`SetCandidateSessionActive` 置位）。已否定：复用 `_hasCandidates`。加词窗一弹出宿主文档就丢焦点，
  `CleanupInputStateForDocChange` → `ResetComposingState()` 会当场把它清零，那几个键又透传给宿主。
- 开关为 `true` 时占位由协调器在进入模式时自己发出，不能指望出口的 `with_composition_placeholder`
  补：那条只在非 `app_inline` 时走，而出厂 `preedit_display = "app_inline"`。零长度 range 回退化矩形
  （height=0）的宿主（如 WPS）上会因此失去坐标来源。
- 出厂只关加词：报障场景就是 Ctrl+= 造词吃掉选中文字；加词弹的是预览窗，对坐标精度不敏感，靶机实测
  关掉后选中内容保住、记事本下预览窗位置仍正确。另几项弹候选列表，错位代价更大且缺实测数据，保持开，
  这也是做成逐模式开关的原因。

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
| 10 | 打到**行满自动回绕**那一刻继续输入编码 | Tabby、WindTerm | 候选窗跟到新行行首；不得先画在行尾再横穿屏幕（实测跳过 1044px） |

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

否决类判据命中时不属于上面任何一条（它们让首显继续等），单独看这几行：

```
caret_probe → 继续等待: 坐标仍等于上一轮权威 (x,y)，宿主尚未 reflow
caret_probe → 继续等待: 坐标仍等于本轮 pre_reflow 帧 (x,y)，宿主尚未 reflow   ← H 类换行
caret_probe → 拒绝: 该宿主 composition rect 恒陈旧，改用组合前空闲上报 ...
组合矩形起点漂移，不采信: (x,y) → 保持本轮锚点 (x,y)                          ← H 类同行漂移
```

⚠ 这三条否决判据**都必须排在连打快路径（`fast_typing_window_ms`）之前**。快路径不比对任何
基准就采信首条采样，排在它下游等于只要按键间隔落进窗口就整条绕过。`pre_reflow` 基准那条尤其
不能放后面——它要救的「行满回绕」正是连打到行尾才会发生的事件，放下游等于挡不住自己的触发条件。

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

6. **候选窗钉在屏幕角落一动不动？** 看 TSF 日志的 `caret 无效(succeeded=? h=0)，降级用组合起点`
   与紧随的 `context GetScreenExt =`：若组合起点不在 `GetScreenExt` 内，是 F 类宿主给的固定垃圾
   坐标，不是我们的定位逻辑出错——别去查重锁/校正那条链。

7. **游戏/UE 宿主里候选窗跑到无关位置？** 先看 TSF 日志有没有成对的
   `GetTextExt failed hr=0x80040505` + `Composition rect (…) w=N h=0`：有就是 G 类，正常情况下
   紧跟着应出现 `二级降级用组合整体矩形 … 补高度后 caret=(…)`；若该行缺失，看同帧的
   `context GetScreenExt` ——越界校验拒掉（`判为垃圾坐标，不采信`）或 `GetScreenExt 不可用`
   都会让二级降级失败关闭，退回兜底坐标。

8. **长按 / 连打时候选窗一次都不显示？** 看 `SelectionChanged` 是否被回声过滤判成「用户移动光标」
   （`since_self_commit` 远大于一次按键间隔）。自提交打点必须覆盖一切真落屏的返回变体（含满码自动上屏的
   `CommitThenDeferComposition`），漏一种就会清掉 `caret_cache_verified`，下一组合 arm 600ms 长兜底，
   而五笔长按时组合寿命仅约 128ms（4 码 × typematic 约 32ms），兜底永不到期。

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
- **拿「上一轮权威坐标」当唯一的 reflow 基准**：终端行满回绕时必然失效——陈旧值停在行尾
  （实测 1432）、上一轮权威在它左边一个字符宽（1401），两者**天然不等**，于是陈旧值被判成
  「已 reflow」放行首显。补救不是给它加距离/方向条件（那就退回上一条的老路），而是**换基准**：
  同一轮里 DLL 已经用 `CARET_SRC_PRE_REFLOW` 标出了重排前的那一帧，probe 与它逐位相同即未重排。
  分界线是「拿 pre_reflow **定位置**」（2026-08-01 翻车，错位被 settle 容差固定）与「拿它
  **否决别人**」（只会多等一轮，方向安全）——两者相反，别混为一谈。
- **组合矩形可信判据写成 `left >= caret.x`**（「起点在插入点左边」）：只在单行成立，换行后 caret
  回到行首恰好等于 `left`，真范围被误判成插入点（Excel 实测 w=402 的矩形被丢弃）。改为按宽度判，
  见 §二·六。
- **大偏移逃生阀拿已锁的 `composition_start` 与宿主上报的 compStart 比**：见 E 类，问宿主前后两帧，
  别问自己。
- **上屏后在 `reset_first_show` 里清掉 `caret_cache_verified`**，让下一组合改等权威坐标：上屏前移了
  插入点、缓存确实陈旧（微信五笔满码上屏后约 20px），但清掉后下一组合走 600ms 长兜底，而长按时
  组合寿命仅约 128ms，兜底每次都被作废、永不到期，候选窗停在几百像素之外。微信的权威坐标要约
  190ms 才到，「等到正确坐标」与「在组合内显示」互斥，取一次性小抖动。
- **靠单个宿主验证**：见第三节。

## 六、相关文件

| 位置 | 作用 |
|---|---|
| `wind-coordinator/src/coordinator/first_show.rs` | 首显闸门、兜底 timer、`absorb_probe_coords` |
| `wind-coordinator/src/coordinator/message_handler.rs` | `handle_caret_update` / `handle_caret_probe` |
| `wind-coordinator/src/coordinator.rs` | `notify_ui_update` 里的位置计算与逃生口 |
| `wind-config/src/app_compat.rs` | per-app 规则（`first_show_mode`、`caret_use_top`、`stale_probe_guard`、`composition_start_pair_guard`） |
| `data/compat.toml` | 出厂 per-app 规则 |
| `wind_tsf/src/CaretEditSession.cpp` | 两级降级（组合起点 / 组合整体矩形）、`GetScreenExt` 越界校验、IMM32 候选位置探测 |
| `wind_tsf/src/TextService.cpp` | `OnAsyncCaretRectReady`、probe 发送与 caret source 标记 |
| `wind-ui/src/candidate_window.rs` | `place_window`：上下翻转决策与垂直可用区（见 §二·五） |
| `wind-keys/src/foreground.rs` | `foreground_covers_monitor`：铺满显示器判据 + cloaked / shell 两道守卫（**唯一实现**，勿重写） |

> ⚠ 用户层 `%APPDATA%\WindInputDev\compat.toml` 的合并语义通常是「同名进程**整条**覆盖系统层」。
> 排查「出厂规则不生效」时先看用户层有没有该进程的条目——菜单改回「跟随全局」曾会留下只剩
> `process` 的空壳条目，把出厂规则整条屏蔽掉（已修：写盘时剔除空壳）。
> `composition_start_pair_guard` 是协议级安全例外：用户层未写时继承出厂值，显式
> `false` 才关闭，因而已有的 QQ 稀疏自定义规则不会让本修复在升级后失效。
