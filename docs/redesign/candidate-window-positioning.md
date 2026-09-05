# 候选窗坐标时序与定位设计

> 记录候选窗"在哪里显示"这件事背后的 Windows TSF 复杂性与防抖/防错位机制。
> Go 版本（`../WindInput/wind_input`）经长期实测稳定，Rust 版本据此移植。
> 涉及 DLL（`wind_tsf`）、协调器（`wind-coordinator`）、UI（`wind-ui`）三层。

> ⚠ **动手改本文档描述的任何判据之前，先读
> [`docs/architecture/caret-position-host-compat-notes.md`](../architecture/caret-position-host-compat-notes.md)。**
> 那里有各类宿主的实测画像、必测矩阵和已被推翻的方向——本设计的正确性高度依赖宿主行为，
> 而不同类型的宿主（表格类 / Qt WebView / 纯文本编辑器 / 终端）会以完全不同的方式违反假设，
> **单个宿主上验证通过完全不能说明问题**。

## 1. 问题现象

候选窗应紧贴输入光标显示。但在某些应用（尤其终端类如 **tabby**、WebView、WPS、Excel）会出现：

- **错位**：输入完一个字/词上屏后，立即输入下一个，候选窗出现在偏离光标约"一个刚上屏内容宽度"的位置。
- **抖动**：候选窗出现时先在一个位置闪现、再跳到另一处；或输入过程中随宿主上报的光标微抖动而抖。

"不是所有应用都出现"是关键线索——根因是**宿主上报光标坐标的时序**，而非定位算法本身。

## 2. 根因：Windows TSF 的坐标采集时序

候选窗位置来自宿主光标矩形，经 `ITfContextView::GetTextExt` 获取（屏幕坐标）。问题在于：

1. **reflow 滞后**：新 composition 刚 `StartComposition` 时，宿主尚未完成文本重排（reflow）。此刻 `GetTextExt` 返回的是**reflow 前的旧坐标**（上一组合/上屏前的位置）。错位量 ≈ 刚上屏内容宽度。
2. **退化矩形**：reflow 未完成时部分宿主返回 `height==0` 的退化矩形，坐标完全不可靠。
3. **坐标微抖**：WPS 等在首次与后续 `GetTextExt` 间返回相差 1~2px 的光标高度/位置；微信等 WebView 的 `height` 在 `1`/`20` 间跳变，使 `rect.bottom` 相差达 20px（但 `rect.top` 稳定）。
4. **坐标系不一致**：个别控件 `GetRange` 让组合起点 anchor 随输入漂移；或返回 logical/physical 混用的越界坐标。

> Windows 输入法栈（TSF + CUAS + 各框架自绘）没有统一的"光标已稳定"信号，只能靠一组经验性 hack 逼近。以下机制都是为对抗上述时序而生。

## 3. DLL 层（wind_tsf）的应对

见 `wind_tsf/src/TextService.cpp`：

| 机制 | 位置 | 作用 |
|------|------|------|
| `_compositionJustStarted` | `StartComposition` 后置位 | 标记"刚启动、reflow 未完成"，推迟首次坐标发送 |
| 推迟 + `SendCaretPending` | `SendCaretPositionUpdate` | justStarted 期间不立即发坐标，先发"坐标待定"握手 |
| `OnLayoutChange` debounce | `OnLayoutChange` | reflow 完成的权威信号；burst 期间 debounce，等稳定后 flush（50ms，首显延迟的大头） |
| 50ms timer 兜底 | — | 应对完全不发 `OnLayoutChange` 的应用（**比预想的多**，见第 6 层的宿主画像：Word / 记事本都不发） |
| `SendCaretProbe` 试探采样 | `OnLayoutChange` 首帧分支 | 首帧 reflow 期间每次 layout change 采一次坐标（限前 5 次）发给服务端，供 `fast` 档提前放行；DLL 不做判断 |
| `height==0` / 越界过滤 | `GetCaretPositionFromTSF` / `_CacheCaretPosition` / `OnAsyncCaretRectReady` | 退化矩形、`IsScreenPointOutsideAllMonitors` 越界坐标不缓存、不上报 |
| 异步 edit session | `RequestCaretRectAsync` → `OnAsyncCaretRectReady` | 非按键上下文取坐标的唯一正确方式，见 3.1 |
| 锚点降级 | `CCaretEditSession::DoEditSession` | selection 退化时用组合起点当 caret，见 3.1 |
| `SendCaretUpdate(x,y,h,compStartX,compStartY)` | reflow 后 | 发送权威坐标 + 组合起点坐标 |

要点：**DLL 保证每个新组合"先发 CaretPending、reflow 后再发权威 CaretUpdate(height>0)"**。协调器据此实现"延迟首显"。

## 3.1 坐标来源、锁模式与可信度（2026-08 重构）

上表的每一格都在回答"怎么把坐标拿到手"。这一节回答更根本的问题：**手上有哪几个坐标来源、
它们分别可信到什么程度、取不到时该向谁降级**。三个真实 bug（Word 标题行错位 814px、
桌面输入定位到任务栏、桌面第二字错位）根因都在这里。

### 三个坐标来源，可信度随场景变化

| 来源 | 回答的问题 | 语义域 |
|------|-----------|--------|
| `GetTextExt(selection range)` | 文本插入点在哪 | TSF context |
| `GetTextExt(composition range)` | 这次组合的头部在哪 | TSF context |
| `GetGUIThreadInfo` | 这个 GUI 线程的系统光标在哪 | Win32 窗口 |

**没有一个来源是恒定可信的**，实测：

| 场景 | selection | composition | GUI caret | 可用的 |
|------|-----------|-------------|-----------|--------|
| Word 正文 / 记事本 | 有效 | 有效 | 大致正确 | 都行 |
| Word 标题等非正文样式行 | 有效 | 有效 | **`rcCaret` 退化，指向别处** | 前两个 |
| 桌面 / 任务栏 / 托盘弹框 | **恒退化 h=0** | 有效 | **别的 shell 窗口残留值** | **只有 composition** |
| 非 TSF 宿主 | 无 | 无 | 唯一来源 | 只有 GUI caret |

> ⚠ **回退链的危险不在"失败"，在"以成功的形式失败"**。`GetCaretPosition` 曾把「拿到了一个坐标」
> 和「拿到了**那个**坐标」压成同一个 `TRUE`，于是 GUI caret 冒充 TSF 坐标，还抢在更可靠的
> `_lastKnownCaretPos` 之前。`h=20`（`DEFAULT_CARET_HEIGHT`）与 `compStart=(0,0)` 是它的两条指纹。

### 锁模式：`TF_ES_SYNC` 只在按键上下文合法

MSDN 对 `TF_ES_SYNC` 的明文限制：*"should only be used in documented situations (such as keystroke
handling) where it can be expected to succeed. Otherwise the call will likely fail."*
锁协议规定，文档被锁时同步请求返回 `TS_E_SYNCHRONOUS`(`0x80040208`) 拒绝，而**异步请求会排队**，
等文档可用再回调。

所以 **WM_TIMER、焦点回调等非按键上下文必须用 `TF_ES_ASYNCDONTCARE`**。曾经用 `TF_ES_SYNC`
的 timer 兜底路径在 Word 上 15/15 全被拒，一直靠 GUI caret 回退在苟——正文行回退值恰好正确
（偏差 2px，被 3px 判据吞掉），标题行就露馅（偏差 814px）。

实测两个宿主走了同一个 `ASYNCDONTCARE` 的两条分支，都成功（"at the discretion of the manager"）：

| 宿主 | `hrSession` | 行为 |
|------|-------------|------|
| Word | `TF_S_ASYNC` (`0x00040300`) | 排队，**1~2ms** 后回调 |
| 记事本 | `S_OK` | manager 选择内联执行，回调在 `RequestEditSession` 内部跑完 |

> 内联执行时 `OnAsyncCaretRectReady` 日志会出现在 `accepted` **之前**，这是正常顺序不是异常。
>
> Weasel 全仓零 `TF_ES_SYNC`、四处 `RequestEditSession` 全用 `ASYNCDONTCARE`，且全仓零 timer
> ——**异步锁本身就是"等"的机制**，不需要自己再造一个更差的排队器。

### 越界判据：参照系必须是"物理存在"，不能是"前台窗口"

原判据 `IsScreenPointOutsideForegroundWindow` 隐含前提「输入目标 ⊆ 前台窗口」。
**shell 场景直接证伪**：输入焦点属于 explorer 的 context（渲染成屏幕一角的临时小输入窗），
而前台窗口可能是桌面、`Shell_TrayWnd`（屏幕底部一条）或托盘图标弹框——**前台窗口与输入目标
是两条独立的线**，点任务栏只改变前者。

实测该判据**误伤 19 次 vs 正确拦截 12 次**：

| 类别 | 实测坐标 | 性质 |
|------|---------|------|
| 焦点窗口 ≠ 前台窗口 | `(13,9)` ×9 | 合法，被误伤 |
| 窗口移动中（`GetTextExt` 与 `GetWindowRect` 取自不同时刻） | `(473,189)` ×10 | 合法，被误伤 |
| 真野坐标 | `(1284,1309)` `(-25563,1198)` `(2199,-1704)` `(669,-3375)` 等 | 应当拦截 |

现判据 `IsScreenPointOutsideAllMonitors` 只问"这个点物理上存不存在"：真野坐标全部远离**所有**
显示器，照样挡得住；两类误伤消失。

- ⚠ 用 `MonitorFromPoint` 而非 `SM_*VIRTUALSCREEN`：虚拟屏幕是所有显示器的**外接矩形**，
  多屏错位排布时屏幕之间有空隙（实测机器副屏 `X∈[-1920,-480]`、主屏 `X∈[0,1707]`），
  外接矩形会放行落在空隙里的坐标。
- ⚠ 本判据比原判据**粗**：它在 DPI 转换前执行，而 DLL 运行在各种 DPI awareness 的宿主里
  （原判据因 `GetWindowRect` 与 `GetTextExt` 同进程同语境而天然免疫）。可接受，因为这里只做
  "离谱与否"的粗判断，真野坐标差几千像素，远超 DPI 缩放的 1.5~2 倍偏差。
- ❌ **`ITfContextView::GetScreenExt` 不能当参照系**：看似是"context 自己的显示区域"这个
  语义上最正确的锚，但 shell context 上实测返回退化矩形 `(0,1368,0,1368)`。已验证，勿重试。

### 锚点降级：selection 退化时用组合起点

候选窗要跟随的本来就是**正在编辑的那段文本**，不是插入点——两者只差一个组合宽度。故：

```
caret(selection) 有效        → 用它
caret 无效但 composition 有效 → 用组合起点当 caret（降级）
两者都无效                   → 才谈回退 GUI caret
```

桌面场景正是靠这一条修好的：selection 恒退化，原先下坠到 GUI caret 取到任务栏残留的
`(0,1388)`（与真实位置差 1171px），服务端进而以"组合起点距 caret ≥500px"把唯一正确的
`(473,217)` 也当异常丢弃——**一个错误的基准值，把唯一正确的数据也判成了异常**。

> ⚠ 组合起点的获取此前被 `if (_succeeded && _pComposition)` 守卫着，潜台词是"caret 都没取到，
> 组合起点也不会有"——而实测正好相反。**一个数据的获取被另一个数据的成败守卫，等于宣告两者
> 强相关**，而这个相关性从未被验证。日志表现为 `Composition start rect` 一行都不出现，看起来
> 像"取不到"，实际是"压根没去取"。

### 来源标记与同源校验（已实施）

TSF 坐标与 GUI 回退坐标是两个语义域，压进同一组 `x/y/h` 字段后下游无法区分。故新增
`CaretPayloadV2`（24 字节 = `CaretPayload` + `source`），`CARET_SRC_*` 覆盖回退链每一级：
`tsf_selection` / `tsf_composition` / `tsf_cached` / `gui_caret` / `console` / `last_known`。

- ⚠ **刻意不把 `source` 加进 `CaretPayload` 本身**：`FocusGainedPayload` 内嵌了它，改大小会
  连带改变焦点载荷布局。服务端按 payload 长度分支——20 字节（旧 DLL）与 12 字节（macOS）
  均落 `UNKNOWN`，故新旧两侧可任意组合。实测旧 DLL 时段 `unknown ×33`，新 DLL 生效后归零。
- 据此收紧组合起点锚定：原「组合起点距 caret ≥500px 即丢弃」的前提是**两者同源**，它要抓的是
  「同一 context 报出两个坐标却相差离谱」这种坐标系不一致。当 caret 本身来自 GUI 回退时两者
  根本不是一个语义域，比较毫无意义——桌面输入实测 `caret=(0,1388)` 是任务栏残留光标、
  `compStart=(473,217)` 才是真实位置，`dy=1171` 让这道闸门把**唯一正确的数据**当异常丢弃。
  现改为非 TSF 源直接采信组合起点，同源时保持原有 500px 保护不变。

> **一个错误的基准值，会把唯一正确的数据也判成异常。** 这类"错值当基准"的连锁失效比单点
> 错误难查得多——500px 那条规则做得很仔细，正因为仔细才稳定地杀掉了正确答案。

## 4. 协调器 + UI 层的三层机制（对齐 Go）

Go 版本用三层独立机制覆盖不同失效模式，Rust 同构移植：

### 第 1 层：延迟首次显示（pendingFirstShow）—— 根治错位

**新组合首帧不立即显示候选窗**，而是等 reflow 后的权威坐标（或兜底超时）再首显。从根本上避免在 reflow 前的陈旧坐标处显示，因此既无错位也无"先显示再跳"。

状态机（Rust：`wind-coordinator/src/coordinator.rs`）：

- 字段：`pending_first_show: Mutex<bool>`、`pending_first_show_token: Mutex<u64>`、`candidate_shown: Mutex<bool>`、`show_authorized: AtomicBool`。
- `notify_ui_update` 门控：过了"无内容/candwin 隐藏"守卫后，若 `!show_authorized && !candidate_shown`（首帧且非授权）→ `arm_pending_first_show()` 并 `return`（不下发）。
- `arm_pending_first_show_with_timeout(ms)`：置 `pending=true`、自增 token、共享定时器线程排兜底 timer（超时按档位取，见第 6 层）。timer 到点比对 token/pending 仍有效则强制首显（用当前坐标，慢应用降级）。
- `handle_caret_pending`：DLL 握手，若正等待首显则把兜底超时延长到 **600ms**（应对 `OnLayoutChange` burst 慢的应用，如 EverEdit）；`fast` 档拒绝此延长。
- ⚠ **`reset_first_show()` 会 bump token 作废未到期的 timer**。它在每次上屏时都被调用，所以兜底超时
  一旦长于组合寿命，timer 就永远等不到自己到期——这是 `fast` 档必须用短兜底的直接原因（第 6 层判据 3）。
- `handle_caret_update`：`height==0` 直接跳过；权威坐标到达且 `was_pending` → `show_authorized=true` 后 `notify_ui_update`（首显落在正确坐标）。
- 下发 `UpdateCandidates` 后置 `candidate_shown=true`；`reset_first_show()` 复位首显状态并作废 timer，调用点：`notify_ui_hide`、`notify_ui_update` 的隐藏分支、`handle_commit_request`（上屏）、**顶码上屏**（`top_code_commit`：部分上屏 + 余码续组合，宿主光标已前移，余码候选窗须重新延迟到新坐标并重锁组合起点）。

时序（上屏后立即输入下一个）：
```
上屏 commit → reset_first_show（candidate_shown=false, 作废 timer）
首键 → notify_ui_update：首帧未授权 → arm（pending=true, 150ms timer），不显示
DLL CaretPending → handle_caret_pending：延长到 600ms
DLL reflow 后 CaretUpdate(height>0, C_new) → was_pending → 授权首显 → 候选窗出现在 C_new
（从不在旧坐标 C_old 显示，故无错位、无跳动）
```

### 第 2 层：3px caret 移动过滤 —— 已显示后防抖

候选窗已显示后，`handle_caret_update` 收到的坐标若与上次相差 `≤3px`（且非首显）→ 跳过 reshow，吞掉宿主 caret 微调（如 WPS 的 2px 偏移）。显著变化（换行/reflow 修正）才 reshow。

### 第 3 层：4px 位置阈值 —— 渲染落位防抖

UI 层（`wind-ui/src/candidate_window.rs`）**每帧据当前光标 + 内容尺寸重算窗口位置**（`place_window`），再与上次内容锚点比较：`<4px*scale` 微移则保持原位（`last_content_pos`）。这是位置保护的最后一道，吞掉穿过前两层的残余微抖。

> 注意：UI 层**不再锁定锚点**（早期 Rust 实现曾用 anchor 锁定，导致首帧锁死在陈旧坐标、reflow 坐标被忽略——正是 bug 之源）。改为"每帧重算 + 阈值过滤"，与 Go 一致。

### place_window 的定位规则（满足若干交互需求）

`place_window(caret_x, caret_y, caret_h, w, h, sticky_above)`（`candidate_window.rs`）：

- 默认显示在光标下方（`caret_y + gap`）；下方空间不足则上翻到光标上方。
- **上方显示以"窗口底边贴光标顶端"为参考**：`above_y = caret_top - h - gap`，底边与高度无关 → 候选变少时顶边下移、底边不动，不会离光标变远。
- **上翻粘滞（sticky_above）**：一旦上翻，候选数量变化也保持上方，仅当上方也放不下才回落（`placed_above` 跨帧维持，隐藏时复位）。
- 左右溢出贴边（横向右方不足时左移）。
- 尺寸变化每帧重算 → 不溢出屏幕。

## 5. 关键文件 / 函数索引

| 层 | 文件 | 关键点 |
|----|------|--------|
| DLL | `wind_tsf/src/TextService.cpp` | `_compositionJustStarted`、`SendCaretPositionUpdate`、`OnLayoutChange`、`SendCaretUpdate` |
| 协调器 | `wind-coordinator/src/coordinator.rs` | `pending_first_show`/`candidate_shown`/`show_authorized`/`composition_start`、`arm_pending_first_show*`/`first_show_fallback_ms`、`reset_first_show`、`notify_ui_update` 门控+坐标基准、`handle_caret_update`、`handle_caret_pending`、`handle_caret_probe`、`handle_commit_request`、`update_active_compat`/`active_compat`/`process_name`、`first_show_was_provisional`/`last_authoritative_caret`/`last_key_interval_ms` |
| 兼容规则 | `wind-config/src/app_compat.rs`、`data/compat.toml` | `AppCompat::load`/`get_rule`（`[[apps]]`：`process`/`caret_use_top`/`first_show_mode`，后者为 `Option`＝可跟随全局），系统层+用户层覆盖；`FirstShowMode` 枚举、`set_user_first_show_mode`（菜单写盘） |
| 菜单 | `wind-coordinator/src/handle_menu.rs`、`wind-ui/src/manager.rs` | `set_first_show_mode`（写盘→重载→刷新 active_compat）、`MenuCmd::FirstShowMode(u8)`（id 段 `5000..=5999`） |
| IPC | `wind-bridge/src/{handler,server}.rs` | `CaretData{ x,y,height,composition_start_x,composition_start_y }`、`CMD_CARET_PENDING → handle_caret_pending`、`CMD_CARET_PROBE → handle_caret_probe`、`client_token`（高 32 位 = PID） |
| UI | `wind-ui/src/candidate_window.rs` | `place_window`（下方 `caret_y+gap`、上方 `caret_y-height-…`）、`last_content_pos` + 4px 阈值、`placed_above` 粘滞 |

### 第 4 层：compositionStart 组合起点锚定 —— 钉在缓冲头部

**嵌入预编辑模式**（`app_inline`：编码插入宿主、宿主光标随输入右移）下，候选窗若跟随当前光标会一直移到输入缓冲末尾。改用**组合起点坐标**锚定，使候选窗钉在缓冲头部不随输入移动：

- `coordinator.rs` 新增 `composition_start: Mutex<(x, y, valid)>`。
- `handle_caret_update`：组合内首个有效 `compStart` 锁定（`!valid` 时才写），后续即便携带新值也不覆盖——防部分控件 `GetRange` 让起点随输入漂移；`<500px` 校验排除 logical/physical 坐标系不一致。
- 首显锚点整体错误时允许按 caret 大偏移重锁。不能全局改成“reported compStart 非零就信它”：
  其它宿主已有陈旧值和坐标系混用历史，会关闭原有自愈。对确认存在稳定帧对的宿主，以 per-app
  `composition_start_pair_guard` 开启窄保护；只有来源顺序、前帧降级点、reported compStart 与当前锁
  四者一致时，才把后续 selection 的大跨度判为正常组合宽度并保持起点。
- `notify_ui_update` 坐标块：`in_app && compStart.valid` → 用 `compStart` 替代当前光标。
- `reset_first_show`（组合结束/隐藏）复位 `valid=false`，下一组合重新锁定。
- **`handle_focus_gained` 也复位**（2026-08-02）：焦点事件意味着换了 DocMgr，见下。
- 非嵌入模式（preedit 显示在候选窗、宿主光标不动）仍用当前光标。

> 局限：组合期间锁定首个 compStart，故宿主窗口在组合中途移动/滚动时候选窗不跟随（与 Go 一致；组合通常很短，影响小）。

#### ⚠ 锚定的隐含前提：组合起点不会移动

Excel 打破了它——输入时会在「单元格」与「公式编辑栏」两个 DocMgr 之间来回切，**组合本身还在
（buffer 未清）但它的宿主位置整体迁移了**（实测 `(593,572)` → `(1457,959)`）。锚点若不作废，
候选窗就钉死在旧 DocMgr 上。

**指纹**：协调器判出 `reshow: dx=1297` 说要重定位，**下发的 UI 位置却纹丝不动**——reshow 拿
`state.caret_*` 判、下发却用锁死的组合起点，两者读的不是同一个值。

故 `handle_focus_gained` 时作废锚定，由下一帧 `caret_update` 就地重锁。

QQNT 提供了另一种反例：reported compStart 始终稳定，但同一按键后先报 selection 无效时的
`TSF_COMPOSITION` 起点降级帧，再报真实的 `TSF_SELECTION` 组合末端。长拼音使末端离起点超过
`3 × line_height` 后，若大偏移重锁以 caret 距离作证据，就会把两帧轮流写进
`composition_start`，候选窗在起点与末端之间闪烁。出厂 `QQ.exe` 规则因此开启
`composition_start_pair_guard`；协调器仅保护完整匹配的帧对，组合宽度增长不构成重锁理由，
其它宿主仍保持原 caret 逃生阀。

> **这个缺陷是被另一个修复"激活"的**：此前 Excel 的 `compStart` 取不到或被距离校验丢弃，
> 锚定**从未真正生效**，候选窗一直跟着 caret 走，只表现为"跳一下"。修好 selection 退化降级与
> 越界判据（§3.1）后 `compStart` 变可靠，反而让这段从未被执行过的逻辑第一次跑起来。
> **「机制终于生效了」和「机制是对的」是两个独立命题**——修好上游时，要把下游那些因上游沉默
> 而空转的分支当作**新代码**看待，它们从没被验证过。

DocMgr 层级的其它同类缺陷（地址栏首字母上屏、焦点气泡时机）见
[TSF 焦点层级与判据选择](../architecture/tsf-docmgr-focus-semantics.md)。

### 第 5 层：应用兼容规则 caret_use_top —— WebView 光标矩形归一化

部分应用（微信 Qt WebView 输入框等）`GetTextExt` 返回的光标 `height` 不稳定：在 `1`/`20px` 间跳变，
导致 `rect.bottom`（= top + height）相差达 ~20px，而 `rect.top` 始终稳定（≤1px，视觉上 ≈ 正文底端）。
若按默认的 `rect.bottom` 定位，候选窗会随 height 跳变上下抖 ~20px。

按进程名匹配的兼容规则（`compat.toml` 的 `[[apps]]`，对齐 Go `pkg/config/compat.go`）解决：

- **规则加载**：`wind-config/src/app_compat.rs` 的 `AppCompat::load(data_dir, user_dir)`——系统层
  `{data}/compat.toml` + 用户层 `{user_config}/compat.toml` 覆盖；`get_rule(process)` 不区分大小写。
  系统预置见 `data/compat.toml`（`Weixin.exe → caret_use_top = true`）。
- **进程识别**：协调器 `update_active_compat(client_token)` 从 `client_token` 高 32 位取 PID
  （`pid = token >> 32`，复用既有 token 编码，无需改 IPC 协议），经 `process_name(pid)`
  （`OpenProcess` + `QueryFullProcessImageNameW`，对齐 Go `bridge.GetProcessName`）解析进程名，
  查规则后把 `(pid, caret_use_top)` 缓存进 `active_compat`；按 pid 缓存避免每帧 `OpenProcess`。
  接入点：`handle_focus_gained`（FOCUS_GAINED 重型后置段，不在 DLL 同步阻塞路径上）、`handle_ime_activated`。
- **坐标变换**（`handle_caret_update` 顶部，对齐 Go `HandleCaretUpdate`）：命中规则时
  `Y -= rawH`（bottom → 稳定的 top）、组合起点 Y 同步上移。

> **height 必须保留真实行高，不能压成 1**（与 Go 的差异点）：wind-ui 的**下方**公式 `below_y = caret_y + gap`
> 不读 height，故下方紧贴只靠稳定的 top；但**上方**公式 `above_y = caret_y - height - hi - gap` 用
> `caret_top = caret_y - height` 推算正文顶端。若 height=1，正文顶端被当成 `top-1`（≈正文底端），
> 上方候选窗会整条压住正文/光标。故变换保留 `height = rawH.max(CARET_USE_TOP_MIN_LINE_H=18)`：
> 真实行高让上方正确避让正文，退化帧（rawH=1）落到下限兜底。偏大只是上方多留空隙，偏小才遮挡——宁大勿小。

### 第 6 层：首显档位 FirstShowMode —— 用延迟换准确的三档取舍

第 1 层根治了错位，但代价是**首显恒定延迟 85~95ms**（C++ `OnLayoutChange` 的 50ms debounce 占大头，
每次 burst 事件都重置它）。连打时组合本身只活几十毫秒，候选窗往往来不及出现就被下一次上屏
`reset_first_show()` 掀掉，表现为「迟钝」。且延迟无法靠单纯调小超时解决——超时短了就退回错位。

出路是承认**这是取舍而非 bug**，按宿主分档。档位由 `FirstShowMode` 枚举表达，**两层**决定：
全局默认档 `config.toml` 的 `ui.candidate.first_show_mode`，per-app 覆盖 `compat.toml` 的
`first_show_mode`（**`Option`，缺省＝跟随全局**）。

| 档位 | 菜单名 | 首帧行为 | 适用 |
|------|--------|----------|------|
| `fast`（**出厂默认**） | 快速显示 | 采信试探坐标 / 连打直接放行 / 短兜底，三条判据见下；坐标不可信时自动退回长兜底 | 通用 |
| `wait` | 等待精确坐标（较慢） | 第 1 层原样：等权威坐标或 150ms 兜底 | `fast` 判据失灵的宿主兜底 |
| `instant` | 立即显示（最快，可能抖动） | 完全不等，用按键前的坐标（走 `notify_ui_update` 逃生口） | 组合期极短、或根本不上报组合坐标的宿主 |
| —（per-app 第四档） | 跟随全局（默认） | 清掉 `compat.toml` 里该字段，用全局档 | 撤销 per-app 覆盖 |

★ **`AppCompatRule.first_show_mode` 必须是 `Option`**：全局默认档可配之后，「这个应用没配过」
与「显式配了恰好等于当前全局的那一档」是两件事——后者不会跟着全局设置一起变。若不区分，
用户改全局默认时所有从未配过的应用都纹丝不动，且无从撤销。

★ **档位的读取统一走 `Coordinator::effective_first_show_mode()`**（`coordinator/first_show.rs`）：
per-app 有值用它，否则回落全局配置（认不出的值再回落枚举 `#[default]`）。
⚠ `ActiveCompat.first_show_mode` **刻意保留 `Option`、不在写入时就地回落**——它是随焦点切换
刷新的镜像态，写入时烘进全局值会得到「设置页改了要切一次焦点才生效」。回落只在读取侧发生。

> **默认档 2026-08-03 由 `wait` 改为 `fast`。** `fast` 此前不敢作默认，是因为焦点切换 / 鼠标移动
> 光标之后的首帧会拿一份属于别处的旧坐标去定位；**首帧信任门**（见下）补上该洞后，它在坐标
> 不可信的那一刻会自动退回去等真值。实测常规连打首帧中位 **7ms**，焦点后首帧中位 **105ms**
> 且位置正确。
>
> ★ 同时要认清 `wait` 的「准」有很大一部分是**碰巧**的：它在 Excel 那类慢宿主上靠
> `caret_pending` 的 600ms 延长兜住，宿主再慢一点就兜不住——实测 Excel 需要 808ms 的那次它
> 就没兜住，照样先错位再跳。**真正解决错位的是信任门，不是等得久。**

`fast` 档的三条判据（`coordinator.rs::handle_caret_probe` / `first_show_fallback_ms`）：

1. **试探采样 + 「≠ 上一轮权威坐标」**：DLL 在首帧 reflow 期间每次 `OnLayoutChange` 取一次坐标发
   `CMD_CARET_PROBE`（限前 5 次）。协调器判断该坐标是否已不等于上一轮权威坐标——不等即说明宿主已
   reflow，本帧可信，立即首显。这一条把 EverEdit 的首显从 ~90ms 压到 ~3ms、WPS 到 ~11ms。
2. **连打快路径**（`fast_typing_window_ms`，默认 100ms）：相邻两次按键间隔小于该值时，跳过第 1 条的
   比对直接采信首条采样。依据是连打时光标沿同一行顺序前移、不发生重排，坐标本就八九不离十，而这种
   节奏下用户对「跟手」的敏感度远高于十几像素的偏差。
3. **短兜底**（`fast_first_show_fallback_ms`，默认 25ms）：等不到试探/权威坐标就用现有坐标先显示。
   **这一条不可省**——见下面的宿主画像：不发 `OnLayoutChange` 的宿主拿不到任何试探坐标，若沿用 `wait`
   档的 150ms，兜底 timer 会在组合结束时被 `reset_first_show()` 作废而**永远不会到期**，`fast` 就静默
   退化成了 `wait`。同理 `handle_caret_pending` 的 600ms 延长对 `fast` 档默认不生效
   （例外见下面的「首帧信任门」）。

#### ★ 首帧信任门：短兜底的前提是「手里的坐标还算数」（2026-08-03）

判据 2、3 都直接采信一份**旧坐标**，它们成立的隐含前提是「上一次记下的坐标 ≈ 当前插入点」。
同一行连打时这个前提很硬——两者只差一个字宽，所以判据 3 从来没出过问题。但它在两种场景下
**整个不成立**，而这两种场景恰好都是「用户刚做完一个动作、正要开始输入」的时刻：

| 场景 | 手里那份坐标是谁的 | 症状 |
|------|------------------|------|
| 焦点刚到达（换 DocMgr / 换应用） | `focus_gained` 随包携带的非权威值，宿主多半还没 reflow | Excel 进单元格第一个字漂移 |
| 用户点击移动了光标（同一 DocMgr 内） | **上一次输入的位置** | 任何编辑器里点一下再打字，第一个字都错位 |

第二种此前从未被报告过，是排查第一种时顺带挖出来的——两者是同一个缺陷：**宿主只在有
composition 时才回送 `caret_update`，所以"光标去哪了"这件事在两次输入之间是断档的。**

故引入 `caret_cache_verified`：坐标缓存**是否已被当前插入点验证过**。

- **置位**：`handle_caret_update` 采纳一帧权威坐标时（与 `last_authoritative_caret` 同一处）。
- **清位**：`handle_focus_gained`；`handle_selection_changed` 的非回声分支
  （该分支的自提交回声过滤 `SELF_COMMIT_GRACE = 200ms` 已由真机日志校准，两类间隔有 2.5 倍余量）。
- **消费**：`arm_pending_first_show` —— `fast` 档若标志未置位，不 arm 25ms 短兜底，
  改 arm `FIRST_SHOW_LONG_FALLBACK_MS`(600ms)。`handle_caret_pending` 对 `fast` 一律不插手，
  保证兜底时长只有这一个真相源。

> #### ⚠⚠ 长等待一旦开始就不得被后续按键重置
>
> 这是本门能否成立的关键，也是它第一版实现的致命漏洞。首显闸门在候选窗显示前对**每一个
> 字母**都会调 `arm_pending_first_show`（`is_first_frame` 一直为真），而
> `arm_pending_first_show_with_timeout` 每次都 bump token 重新计时。若照常重置，用户多打
> 几个字母就把这段等待反复推后 —— **长兜底静默退化回短兜底，错位照旧**。Excel 建单元格
> 编辑上下文要 558ms，其间用户往往已经敲了三五个字母，所以这个漏洞会让修复对真实输入
> 完全无效，而单字母测试却是绿的。
>
> 故用 `first_show_extended` 记住「本轮已进入长等待」，命中即直接 `return` 保持原 timer。
>
> ★ **这正是「兜底 timer 超时长于组合寿命 ⇒ 被 `reset_first_show` 作废而永不到期」那个
> 死结的镜像**——同一个「计时被反复推后」的机制，一次让 `fast` 退化成 `wait`，一次让长
> 兜底退化成短兜底。**凡是「等一段时间再做」的逻辑，都要单独问一句：这段计时会被什么
> 重置？重置它的那件事，是不是恰好高频发生？**
>
> 反过来，长等待到期后**不再续**（用旧坐标首显仍优于候选窗一直不出现）。

> **为什么判据挂在 arm 时刻**：那是按键的同步响应路径，两个清位信号（`focus_gained` /
> `selection_changed`）都必然早于它到达，判据取值确定。而 `caret_pending` 握手与 25ms 短
> 兜底谁先到无法保证（IPC 往返 + DLL 侧组合创建时机），挂在那里会让行为随时序摇摆。

> #### ⚠⚠ 首显有多条通路，否决判据必须每条都接
>
> 信任门第一版只接在兜底 timer 上，实测（Excel）**闸门刚 arm 600ms 长兜底，6ms 后就被
> `caret_probe` 绕过**：它用 `(1299,535)` 抢先首显，而 200ms 后真坐标是 `(1344,744)`。
> 长兜底形同虚设。
>
> 首显的全部通路（改任何「要不要显示」的判据都要照这张表逐条过）：
>
> | 通路 | 位置 | 是否受信任门约束 |
> |------|------|-----------------|
> | 闸门逃生口 `instant` | `notify_ui_update` | 否——`instant` 的语义就是不等 |
> | 闸门逃生口 `coords_ready` | 同上 | 无需——焦点/上屏都会清 `composition_start`，首帧必不成立 |
> | 兜底 timer 到期 | `fire_pending_first_show` | 是（`arm` 时选长兜底） |
> | **`caret_probe` 提前首显（判据 1 与判据 2）** | `handle_caret_probe` | **是** |
> | `caret_update` 权威坐标 | `handle_caret_update` | 否——这正是我们在等的那一帧 |
>
> ★ 更要紧的是**为什么** probe 必须让位：它的判据在缓存失效时不是「判错了」，而是
> **结构性失去判断力**。判据 1 靠「≠ 上一轮权威坐标」推断宿主已 reflow，可焦点刚切换时
> 那个基准属于另一个单元格，probe 值当然不等于它 ⇒ 判据**恒成立** ⇒ 必然采信一个还没
> reflow 的坐标。判据 2 的「上次按键间隔」跨了焦点，同样说明不了当前帧可信。
> **一个恒成立的判据不是宽松，是没有判断力。**
>
> 这也印证了 `caret_cache_verified` 当初刻意不复用 `last_authoritative_caret.2` 的决定：
> probe 需要的是「基准可比」（＝前者），而后者从不清零、跨焦点仍为 `true`，正是本缺陷的成因。

#### 实测：宿主重排期间的坐标可得性（2026-08-03 观测实验，结论已用完）

一度以为 Excel 在重排期间「给不出坐标」，于是几轮都在调「等多久」。为了终结猜测，做过一次
纯观测实验：组合启动后用**独立线程**按 60/150/300/500/700ms 的节拍 `PostMessage`（不用
`SetTimer`——`WM_TIMER` 是队列里优先级最低的合成消息，只在队列空闲时生成），每拍同时记录
UI 线程响应延迟、`GetGUIThreadInfo` 的 Win32 caret、以及异步 edit session 取到的 TSF 坐标。

| 问题 | 实测答案 |
|------|---------|
| 宿主重排期间还跑消息循环吗 | **跑**。首拍 `lag=140ms`，其余 `lag=0ms` |
| TSF 何时给出正确坐标 | 一轮约 **200ms**（`(1299,535)`→`(1344,744)`），另一轮 60ms 就已正确 |
| Win32 caret 是否更早到位 | **否**，与 TSF 值始终一致（`h` 差 1 是取整）。「用 GUI caret 抢快」这条路排除 |

> 由此**推翻了此前的推断**：曾从一份日志的 680ms 空白推断「Excel 重排期间给不出坐标」，
> 实际那 680ms 是冷启动特例，且空白的真正原因是我们**一次都没问**（唯一的追问机制
> `WM_TIMER` 在那次的忙碌窗口里没排上号）。
> ★ **把「没去做」误当成「做了但拿不到」——这是几轮方向偏差的总根源。**
>
> 观测代码结论已用尽即移除（每次组合启动 5 次 `PostMessage` + 5 次 edit session 是实打实
> 的开销）。需要重做时按上述方法即可，要点：**只写日志、不进任何 IPC 通道**（曾有一次
> 「纯观测」走了 `CMD_CARET_PROBE`，以为 `wait` 档忽略就零风险，结果 `fast` 档同样读它）。

★ **这条判据回答的是「我手里的值可不可信」，不是「未来会不会有更好的值」**——后者正是
2026-08-02 否决的「焦点会话代次」方案，它需要的信息在决策时刻（兜底到期）尚不存在。
**「这个判断做不出来」和「这个判断做错了」是两回事**：前者加再多元信息也无解，只能改变
决策时机或换一个决策依据，本方案属后者。

★ **代价被限制在「每次动作后的第一个组合」**：一次成功的 `caret_update` 就把标志置回，
后续输入全走 25ms 快路径。而"刚点完鼠标/刚切完窗口"本来就有几百毫秒的人类反应时间垫着。
⚠ **它对 `wait`/`instant` 一字不改**：`wait` 的长兜底由握手负责，两条延长路径叠加会让它
最坏等到 1200ms。

**残余代价（需实测确认）**：若某宿主在焦点后**始终不发**组合坐标，该轮组合最长要等 600ms
才显示候选窗；用户若在 600ms 内就上屏，这一轮候选窗不出现。判断依据是日志里
「坐标缓存未经当前插入点验证，改 arm 600ms 长兜底」之后是否紧跟着
「兜底 timer 到期 → 用现有坐标首显」——**成对出现即说明该宿主没给坐标**，届时按宿主调值。
`dd3c836` 的异步 edit session 已让四个实测宿主都能在 1~52ms 内给出坐标，故预期极少命中。

#### 宿主画像（实测，AutoHotkey `d`+空格 循环 50 轮）

| 宿主 | 组合期发 `OnLayoutChange` | 组合坐标到达延迟 | 说明 |
|------|--------------------------|------------------|------|
| EverEdit | 会，burst 3~4 次 | **3~10ms**（试探） | `fast` 档最理想的宿主 |
| WPS | 会 | ~7ms（试探，前两次仍是旧坐标） | 需靠判据 1 跳过旧坐标帧 |
| **Word** | **50 轮 0 次** | ~~60~190ms~~ → **1~2ms**（见下） | 坐标靠 C++ 50ms timer + 异步 `GetTextExt` |
| 记事本 | 几乎不发（仅首轮 1 次） | 内联执行，即时 | 组合期间不发 `OnLayoutChange`，但异步 edit session 拿得到 |

> **2026-08 订正**：原表「Word 组合坐标延迟 60~190ms（edit session 排队极慢）」**测的是同步
> edit session 被拒后走 GUI caret 回退链的表现，不是宿主真实速度**。改用 `TF_ES_ASYNCDONTCARE`
> 后（见 3.1），Word 排队授予锁只要 **1~2ms**，记事本干脆内联执行。
> **慢的从来不是宿主，是那条注定失败的请求。**

> **教训**：`OnLayoutChange` 不是普遍可依赖的信号。任何「等某个宿主回调」的策略都必须自带
> **短于组合寿命**的兜底，否则在不发该回调的宿主上会静默退化成上一档，而日志里只表现为
> 「一直在等」——没有任何一行报错。

#### 首显容差：抖动来自校正动作，不是坐标偏差

`fast`/`instant` 用的是非权威坐标，随后真权威坐标到达时若照第 2 层的 3px 判据必然触发 reshow，
表现为「显示后跳一下」——**而抖动的观感恰恰来自这个校正动作，不是十几像素的偏差本身**。
故引入 `first_show_settle_ratio`（默认 **0.8**）：本轮首显用过非权威坐标时（`first_show_was_provisional`），
该轮**第一次**权威坐标只要偏差在 `行高 × 0.8` 以内就不校正。换行/重排的偏差通常 ≥2 个行高，远超
此阈值，仍会正常校正。多数商业输入法也是这么处理的。

置位该标志的三条路径：`instant` 逃生口、`fast` 的试探采信、以及兜底 timer 到期首显（用的都是旧坐标）。

#### 入口

- **右键菜单**：「应用独立配置（<进程名>）」→「候选窗首显」子菜单**四档**单选（跟随全局／快速／
  等待精确坐标／立即），写用户层 `compat.toml` 并热重载整表（`handle_menu.rs::set_first_show_mode`，
  三步：写盘 → 重载 → 刷新 `active_compat`）。
  ★ 菜单项与 setter 的 id 解析**共用 `Coordinator::FIRST_SHOW_MENU` 一张表**：两处手写时
  「把 id 2 与 3 写反」编译过、测试绿，只表现为点错档位。
- **设置页**：外观 → 候选窗口 → 「首次显示时机」（全局默认档，wind-setting 仓的 manifest）。
- **配置**：全局 `config.toml` `[ui.candidate] first_show_mode`；per-app `compat.toml` 的同名字段
  （缺省＝跟随全局）；另有三个内部选项在 `[ui.candidate]` 下不进设置页
  （`first_show_settle_ratio` / `fast_typing_window_ms` / `fast_first_show_fallback_ms`）。

## 7. 已知降级与未移植项

- **慢应用兜底**：若 reflow 坐标在超时内未到达，兜底 timer 用按键前的当前坐标首显——可能短暂错位后被
  后续 reflow 坐标 reshow 纠正（`fast`/`instant` 下受 0.8 行高容差保护，多数不会真的 reshow）。属可接受降级。
- **`fast` 档在极端连打下仍可能不显示**：Word 在 AutoHotkey `sleep 60` 节奏下组合只活 27~57ms，
  25ms 兜底加 IPC 往返约 30ms，最短的那批赶不上。真人打字节奏（>100ms）不受影响。若要进一步压，
  只能调小 `fast_first_show_fallback_ms`（代价是更常用到旧坐标）。
- **`fast` 档对宿主的依赖未消除**：判据 1、2 都要求宿主发 `OnLayoutChange`；不发的宿主实际是靠判据 3
  退化成 `instant` 在工作，「快速」二字对它们名不副实。
  > **2026-08 部分解决**：那条"不依赖 `OnLayoutChange` 的坐标来源"已经找到——异步 edit session
  > （3.1），它在 Word 上 1~2ms 回调、记事本上内联执行。但 `fast` 档的判据 1、2 目前仍只读
  > `CMD_CARET_PROBE`（由 `OnLayoutChange` 驱动），**尚未接入这条新来源**；不发该回调的宿主
  > 依然只能靠判据 3。接入后 `fast` 档才名副其实。
- **~~`fast` 档在 Excel「进应用后第一次输入」会漂移~~**（2026-08-03 已修，见第 6 层「首帧信任门」）：
  Excel 首次输入要先切焦点、建立单元格编辑上下文（`sameDoc=0`），实测耗时 **454ms**，真坐标
  558ms 才到；而 `fast` 只等 25ms，于是用按键前的旧坐标首显。
  > **`wait` 档在这个场景下不是靠判据赢的，是碰巧被 600ms 兜住了**——Excel 若再慢 50ms 它一样漂。
  > 这正是最终没有采纳「Excel 用 `wait`」这条零代码出路的原因：它治的是症状，且赌的是
  > 「600ms 一定够」。
  >
  > 当时列的三条出路：① `focus_gained` 时清 `first_show_was_provisional`，使随后的权威坐标必然
  > 纠正（明确跳一下，但不会停在错位置）；② `focus_gained` 时重新 arm 首显；③ Excel 用 `wait`。
  > 最终实现是 ② 的一般化——**触发条件从"焦点到达"上升为"坐标缓存未经当前插入点验证"**，
  > 于是同一处改动连带修掉了从未被报告的「点击移动光标后首字错位」。
  > ⚠ ② 当时被评为"等于反转原始取舍"，实际不然：它只推迟**每次动作后的第一个字**，
  > 而非每一次组合。
- **❌ 「组合启动即发起异步 edit session」不能作为 `fast` 档的快速坐标来源**（已实测否定，勿重试）：
  绝大多数宿主对该请求选择**内联执行**（`accepted` 分布 inline 170 : queued 38），等同于同步取，
  拿到的是 reflow **前**的坐标（Excel 实测与随后权威值差 16px）。
  > ⚠ 该探测一度走 probe 通道上报，以为「`wait` 档忽略 ⇒ 零风险」，结果 **`fast` 档同样读 probe**：
  > 本探测比 `OnLayoutChange` 的采样早约 19ms 到达，抢先被判据 2 采信提前首显，随后真权威坐标的
  > 16px 偏差又被 `settle` 容差吞掉，**错位就此固定**，比原先更稳定地错。
  > **「某档位忽略它」不等于「所有档位都忽略它」——往多消费者通道上加生产者，必须逐个消费者
  > 过一遍。** 另注意 `settle` 容差是防抖设计，却会**保护错误的初值**，这是"提前首显"类优化的
  > 固有代价。
- **pendingReplay（跨焦点 buffer 重放）**：Go 对 Excel 单元格/编辑栏切换等有专门的 replay 路径，Rust 暂未引入。

## 8. 调参

- `arm_pending_first_show` 超时：`wait`/`instant` 档 **150ms**、握手延长 `FIRST_SHOW_LONG_FALLBACK_MS`
  = **600ms**；`fast` 档 `ui.candidate.fast_first_show_fallback_ms`（默认 **25ms**，坐标缓存已验证时
  拒绝 600ms 延长；未验证时经首帧信任门延长一次）。
- 连打判定窗口 `ui.candidate.fast_typing_window_ms`（默认 **100ms**，0 = 关闭该快路径）。
- 首显容差 `ui.candidate.first_show_settle_ratio`（默认 **0.8** × 行高，0 = 关闭）。
- 第 2 层 caret 过滤阈值 **3px**（`handle_caret_update`；容差生效时取两者较大值）。
- 第 3 层位置阈值 **4px × DPI scale**（`candidate_window.rs`）。

前三项取自本仓 Windows 实测（见上方宿主画像），后两项取自 Go 实测经验。
调小→更跟手但更易抖；调大→更稳但大幅移动可能滞后。
