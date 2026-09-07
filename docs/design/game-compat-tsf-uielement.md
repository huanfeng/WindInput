# 游戏兼容：TSF UI-less（UIElement）与独占全屏

> 全屏游戏里弹自己的候选窗，轻则看不见、重则把游戏踢出独占全屏（画面闪黑、分辨率来回切，
> 处理不好的游戏直接崩）。TSF 为此定义了 **UI-less 模式**：宿主接管候选绘制，输入法只交数据。
> 本文记录外部规范要点、仓内现状、设计取舍与实施状态。
>
> 相关：`wind_tsf/src/TextService.cpp` UIElement 段、`wind-coordinator/src/handle_uielement.rs`、
> `wind-ipc/src/protocol.rs` 的 `CMD_UIELEMENT_*`；工具栏的全屏隐藏见 `is_foreground_fullscreen`。
>
> ⛔ **Dota 2（起源2引擎）不在本方案的可达范围内，别再为它调 UIElement 的数据形状。**
> 它按输入法**身份**白名单决定要不要画候选，与我们交什么数据无关。见 §1.1。

> 状态：P1（UI-less 数据通道 + 服务端按 pid 不弹窗）与 P2（D3D 独占全屏不弹窗）已实施，
> **未真机**（需要一个走 UI-less 的宿主，见 §7）。P3 为设计备选。

## 1. 问题

两类游戏，两种失败：

| 宿主 | 现状 | 后果 |
|---|---|---|
| **走 TSF UI-less** 的游戏/引擎（SDL2、Unreal、ImeSharp/MonoGame、Win8+ 搜索框） | DLL 已注册 `ITfCandidateListUIElement`，但 getter 全是占位（count=1、"…"），且 `pbShow=FALSE` 后服务端候选窗照弹 | 宿主画出来一条"…"；我们的窗仍盖在游戏上 |
| **不走 UI-less** 的游戏（IMM32 桥接 / 只读组合串） | 服务端候选窗照弹 | 独占全屏下窗口盖不上去，反而把游戏踢出独占态；无边框全屏下正常 |

### 1.1 ⛔ Dota 2 / 起源2引擎：按身份白名单，做不到

**结论：不改名就画不出来，且改名不可接受。** 这一条已耗掉十余轮真机对照，务必先读完再动手。

Dota 2 的 IME 支持在 Valve 自己的 `imemanager.dll`（`game/bin/win64/`）里，它**不读 TSF UI
元素**——走的是 IMM32 老路：`ImmGetContext` → `ImmGetCandidateListW` → `ImmGetCompositionStringW`。
进入这条路之前有一道身份闸门：DLL 里硬编码了一张已知输入法表，按注册表中该输入法的
**TSF Profile Description** 做**全等**比对（`V_stricmp_fast` / `V_wcsicmp`，不是子串匹配）。

```
HKLM\SOFTWARE\Microsoft\CTF\TIP\{CLSID}\LanguageProfile\0x00000804\{profile}
    Description = REG_SZ        ← 比对的就是这个值
```

表里的简体中文条目（2026-09-06 从二进制原样提取）：

```
中文(简体) - 微软拼音输入法      中文 (简体) - 搜狗拼音输入法
中文 - QQ拼音输入法              中文 - QQ五笔输入法
中文 (简体) - 谷歌拼音输入法     微软王码五笔86版 / 98版
中文 (简体) - 加加输入法5.0      中文 (简体) - 念青繁體五筆 2.03
中文 (简体) - 手心… ✗（不在表里）
```

命中 → 专用处理对象，在 `WM_IME_NOTIFY(IMN_CHANGECANDIDATE)` 里**同步**取候选、游戏自己画。
未命中 → 兜底对象在第一道闸门就返回，消息落到 `DefWindowProc`，而
`DefWindowProc(WM_IME_NOTIFY)` 正是把候选转交给**默认 IME 窗口**的那条路。
**「左上角那个小窗」与「游戏里没有候选」是同一处的两个后果**，不是两个 bug。

**证据**（`wind_tsf.dota2.42156.log`，同一进程内旁观 sink 同时记录两家）：

| | 我们 | QQ五笔 |
|---|---|---|
| `[SDL_app]` 收到 `IMN_CHANGECANDIDATE` | 6 send + 9 post | 7 send + 5 post |
| 宿主 `ImmGetCandidateListW` | **0 次** | 20 次 |
| 处理后的动作 | 嵌套发给 `[IME]`（= `DefWindowProc`） | 当场取候选、返回 0 |

七个输入法、三种表现，与白名单**零例外**对上：QQ五笔 / 微软拼音 / 微软五笔 / 搜狗在表内且正常；
冰凌（`冰凌输入法`）、小狼毫（`小狼毫`）、我们（`清风输入法`）全不在表内且全都是小窗 + 无候选。

**⛔ 已实测证伪、别再试的方向**（每条都真机跑过）：

- 改 `ITfCandidateListUIElement` 的任何数据形状——`GetCount` 大小、`flags`（0xF / 0x3E / 0x3F）、
  `GetPageIndex`、`GetSelection` 绝对 vs 页内、`GetCurrentPage`。**宿主对我们的
  `ImmGetCandidateListW` 是零次调用,它从来没看过这份数据。**
- `IsShown` 回 TRUE。QQ五笔（能画）报的是 `IsShown=0`。
- `GetDocumentMgr` 回 `E_NOTIMPL` / `S_OK+NULL` / 真实焦点文档。能画的两家里
  QQ五笔回 NULL、微软拼音回非空——**能画的样本彼此就不一致，不可能是判据**。
- 摘掉 / 挂上 `ITfIntegratableCandidateListUIElement`。微软五笔根本不实现它。
- `BeginUIElement` 时交空列表 vs 交数据。五笔交全表、搜狗只交当页，两家都能画。
- 换候选元素的 GUID 去冒充别家。
- **在同一 CLSID 下多注册一个隐藏（`Enable=0`）的白名单 profile**，主 profile 保持真名。
  2026-09-06 实测不成立：Valve 只认**当前激活**那个 profile 的描述，不遍历同一 TIP 的其它 profile。

**唯一有效的办法**是把注册表里的 Profile Description 改成表中某个串（已实测：改完候选立刻
以游戏风格正常显示）。但那等于在语言栏/Windows 设置里冒用别家产品名，**不作为出厂行为**。
可能的出路只有：向 Valve 提交加入白名单，或做成默认关闭、用户知情的显式开关。

**方法论教训**：当「能工作的样本」在某个维度上彼此都不一致时（此处 `count`、`flags`、
`GetDocumentMgr` 三项，能画的几家各不相同），这个维度必然不是判据——应当立刻转向
「判据不在数据里」，而不是继续在该维度上试值。前九轮就是没做这个转向。
另：判据类排查中，日志分段必须以**状态切换的因果事件**（`Deactivate` / `ActivateEx` 日志行）
为锚点，不能按时间戳估算——切错段会让两段互相借到对方的证据，凭空造出不存在的差异。


### 1.2 ✅ CS2：两道闸门，前一道是 Trusted Mode（已解决）

**结论：把 TSF DLL 部署进系统目录并保持代码签名即可，原因不是 §1.1 的白名单。**
表现是「输入法完全不被加载」，和 Dota 2 的「加载了但不给画候选」是**不同层**的两件事，
别拿 §1.1 的结论套。两道闸门**串联**，都要过；Dota 2 只有第二道，CS2 两道都有。

下面 §1.2 记录闸门机制的定案过程，**§1.2.1 是最终判据与解法**（含一次被推翻的错误结论）。

2026-09-07 静态对照 + 游戏自身日志定案。CS2 的 IME 支持同样在 `imemanager.dll` 里，
两边 DLL **字节数完全相同**（242840），哈希不同（同源不同构建）。白名单逐字比对：

```
CS2  : 54 条含中文的 UTF-8 串
DOTA2: 54 条含中文的 UTF-8 串
差集 : 仅 CS2 有 —— 无；仅 DOTA2 有 —— 无
```

**⇒ 两款游戏共用同一张 IME 白名单，零差异。** 我们选用的兼容别名
`中文 (简体) - 郑码`（CS2 侧偏移 `0x0002b578`）在 CS2 里同样在表内。
注意白名单是 **UTF-8 窄串**，不是 UTF-16 —— 按宽串扫会扫出一堆 ASCII 被两两误读成
CJK 的假货（`浩浥湡条牥搮汬` 其实是 `imemanager.dll`），别被这种噪声带偏。

但 CS2 在 DLL 加载那一层多了一道 **Trusted Mode**，我们**走不到白名单那一步**。
证据是游戏自己写进崩溃转储的日志：

```
20(8.076828):  WARNING: File verification failure. Unknown foreign dll.
               Denied a request to load '\??\C:\Program Files\WindInputDev\wind_tsf_dev.dll'
DLL load denials: 1, last '\??\C:\Program Files\WindInputDev\wind_tsf_dev.dll'
```

判定依据是 `game/bin/win64/` 下三份签名名单（文案 `WARNING: %s Denied a request to load '%s'`
出自共享的 `tier0.dll`）：

| 名单 | 条数 | 形式 | 管什么 |
|---|---|---|---|
| `csgo.signatures` | 116 | `路径~SHA1:…;CRC:…` | 游戏自身文件的完整性 |
| `system.signatures` | 5 条 `AUTH` | `AUTH:证书指纹,颁发者指纹` | 允许的签名者 |
| `foreign.signatures` | 12 条 | `文件名~SIGN:指纹;ISS:颁发者` | **特批的第三方 DLL** |

`foreign.signatures` 全表只有 `MpOav.dll`（Windows Defender 反病毒扫描接口）与
`NvCameraWhitelisting64.dll`（NVIDIA），且整份文件末尾带 `DIGEST:` 自签名，改不了。

**Dota 2 为什么没拦？** 它的 `bin/win64/` 只有 `dota.signatures` + `system.signatures`，
**没有 `foreign.signatures`**。`tier0.dll` 是共享引擎库，那句拒绝文案两边都在，
但只有 CS2 真的启用了这套 —— `foreign.signatures` 存不存在就是这个开关的指纹。

#### 1.2.1 ✅ 判据是「系统目录 **AND** 代码签名」，两个条件缺一不可

**⛔ 本节曾记「CS2 无解、只能等 Valve」，那个结论是错的，已于 2026-09-07 当日推翻。**
错因是当时手上**只有失败样本**：三个被拒的输入法怎么比都比不出判据。拿到 QQ五笔这个
**能工作的正例**后，单变量立刻显形。

真正的对照证据在 `Steam\userdata\<id>\730\local\cfg\trustedlaunch.cfg`——
游戏每次启动就写，记录每个被拒 DLL 的完整路径，**不需要游戏崩溃**（比翻崩溃转储好找得多，
应该先看这个文件）。四方对照：

| 输入法 | 位置 | 代码签名 | 在 `AUTH` 名单内 | CS2 |
|---|---|---|---|---|
| QQ五笔 | `System32\IME\QQWubiTSF\` | Tencent，有效 | ❌ | ✅ **可用** |
| 小狼毫 weasel | `System32\` | ❌ 未签名 | ❌ | ❌ 被拒 |
| 冰凌 | `Program Files\Iime\` | Keroro，有效 | ❌ | ❌ 被拒 |
| 清风（改造前） | `Program Files\WindInputDev\` | Certum，有效 | ❌ | ❌ 被拒 |

**⇒ 判据是「位于系统目录」AND「有代码签名」，与那 5 条 `AUTH` 无关。**
佐证：微软自家 inbox IME（`CN=Microsoft Windows`，指纹 `DC91E564…`）也不在那 5 条里，
却显然能在 CS2 里打字——若 `AUTH` 名单是 IME 的通行证，全体中日韩玩家都没法在游戏里聊天，
Valve 不可能这么设计。**别再拿那 5 条当判据。**

改造后（DLL 部署到 `System32\IME\WindInput[Dev]\` + Certum 签名）实测：CS2 内输入正常，
候选窗正常，且 §1.1 的 Dota 2 兼容别名在 CS2 上一并生效（两道闸门都过）。

**⛔ 仍然无效、别再试的方向：**

- **改 Profile Description（§1.1 的办法）单独用在 CS2 上无效**，因为在改造前 DLL 压根
  没进程内。必须先过 Trusted Mode 这道。
- **往 `.signatures` 里自己加行**：两份名单末尾的 `DIGEST:` 是 Valve 私钥对整份名单的
  签名，改了必然校验失败。
- **指望进那 5 条 `AUTH`**：见上，它根本不是 IME 的通行证。

**用户自行加 `-allow_third_party_software`** 仍然是一条路（`cs2.exe` 里确有此参数，与
`-trusted` 成对），但代价是游戏进入不受信任状态、影响 VAC 保护的服务器 ——
既然系统目录方案已经解决问题，**不作为我们的建议，也不写进设置页**。

**方法论（两条）**：

1. 「完全不被加载」这类症状要先问「**是谁拒绝的、有没有留下话**」，再去猜机制。
   本节全部结论来自游戏自带的日志与名单文件，没有动用任何逆向。
2. ★ **只有失败样本时无法定位判据，必须去找一个能工作的正例。**「找不到出路」
   不等于「无解」——第一版结论就是在缺正例的情况下过早收敛的。

顺带：那份崩溃转储的崩溃原因是 `pak01.vpk is corrupt`（游戏文件损坏），
与本输入法无关，别把两件事并成一件。


## 2. 外部规范要点（已核对）

来源：Microsoft Learn「UILess Mode Overview」、`ITfUIElementSink::BeginUIElement`、
`ITfCandidateListUIElement`、`ITfIntegratableCandidateListUIElement`；参照实现：微软
SampleIME `CandidateListUIPresenter.cpp`、Weasel `WeaselTSF/CandidateList.cpp`；宿主侧消费方式：
SDL2 `SDL_windowskeyboard.c`（`UILess_GetCandidateList`）。

- **宿主怎么声明**：`ITfThreadMgrEx::ActivateEx(…, TF_TMAE_UIELEMENTENABLEDONLY)` 建 UI-less 线程
  （只激活实现了 `ITfTextInputProcessorEx` 且归类 `GUID_TFCAT_TIPCAP_UIELEMENTENABLED` 的 TIP），
  并 advise `ITfUIElementSink`；`BeginUIElement` 里回 `pbShow=FALSE` 即宿主自己画。
  SDL2 三个 sink 方法一律 `*pbShow = FALSE`。
- **TIP 义务**：显示任何 UI 前先 `ITfUIElementMgr::BeginUIElement`；回 FALSE 后**必须**
  `UpdateUIElement`（宿主到那时才读内容，首次 `GetUpdatedFlags` 应全位置位）；回 TRUE 可不调
  Update，但 `EndUIElement` 必须调。`ActivateEx` 带该标志时 TIP「已经知道」线程不要它的 UI，
  可直接省掉。
- **候选列表接口语义**：`GetCount` 是整条列表长度；`GetPageIndex(pIndex,uSize,puPageCnt)`
  给每页起始下标（宿主惯常先传 NULL 取页数）；`GetCurrentPage` 当前页；`GetSelection` 无选中回
  `S_FALSE`；分页应按「页」推进而非滚动，页索引在列表存续期间不该变。
  SDL2 的算法：`pgstart = idx[page]; pgsize = min(count, idx[page+1]) - pgstart`，再逐条 `GetString`。
- **`ITfUIElement::Show(FALSE)`**：宿主中途接管；TIP 可转 Hide 态继续 Update，或 EndUIElement。
- **`ITfCandidateListUIElementBehavior`**：`SetSelection / Finalize / Abort` 由宿主调，语义即
  选高亮 / 定稿 / 放弃。`ITfIntegratableCandidateListUIElement`（ctffunc.h，Win8+ 搜索框）为可选扩展。
- **游戏侧的坑**（Microsoft Q&A #56863，ImeSharp 作者）：IMM32 的 `WM_IME_SETCONTEXT lParam=0`
  在 Win10 2004 上有 bug；TSF UI-less 在他们的实测里工作良好——这正是主流引擎选 UI-less 的原因。

## 3. 仓内现状（实施前）

- `CTextService` 多继承 `ITfCandidateListUIElementBehavior`，`NotifyCandidatesVisibilityChanged`
  已按候选有无调 `Begin/Update/EndUIElement`——当初目的是让 Chromium / QQNT 把我们当"现代 IME"
  走 IME-first 调度（`GetCount` 回 1 就是为此）。`pbShow` 的返回值只存不用。
- 候选列表只在服务进程（`State.candidates`），DLL 手里没有；DLL↔服务是同步请求/应答 +
  独立 push 管道。
- `ActivateEx` 的 `dwFlags` 只记日志。
- 工具栏已有全屏隐藏（`is_foreground_fullscreen`，判据①通知状态 + 判据②矩形铺满），候选窗没有。

## 4. 设计

### 4.1 数据通道：拉取模型

三条命令（`wind-ipc/protocol.rs` ↔ `wind_tsf/include/BinaryProtocol.h`）：

| 命令 | 方向 | 同步 | 内容 |
|---|---|---|---|
| `CMD_UIELEMENT_STATE 0x0217` | DLL→核心 | 异步 | `pid u32 + flags u32`；bit0 宿主接管、bit1 UI-less 线程 |
| `CMD_UIELEMENT_QUERY 0x0218` → `CMD_UIELEMENT_PAGE 0x0219` | DLL→核心 | 同步 | 候选快照：`selected/pageSize/currentPage/count + count×(len u16 + UTF-8)` |
| `CMD_UIELEMENT_ACTION 0x021A` | DLL→核心 | 异步 | `action u32 + arg u32`：SetSelection(绝对下标) / Finalize / Abort / SetPage |

**为什么是拉而不是把候选塞进按键应答**：应答帧各有变长尾（组合串等按「剩余字节」取），没有
位置放可选尾段；改帧格式要动所有解析点。拉取只在**宿主接管时**才发生——不接管的宿主（绝大多数）
键路径零变化、零成本。代价是接管宿主每次候选变化多一次同步往返（命名管道，亚毫秒）。

**为什么不走 push 管道**：宿主在 `UpdateUIElement` 回调里**同步**读 `GetString`，数据必须在
调用前就位；push 是另一个线程、另一条管道，还要再 Post 回 TSF 线程，时序和 SHM 帧一样难对。

### 4.2 DLL 状态机（`TextService.cpp` UIElement 段）

- `_uiHostDraws` = 宿主意愿：`BeginUIElement` 回 `pbShow=FALSE` / 之后 `Show(FALSE)` 置真，
  `Show(TRUE)` 置假；EndUIElement **不清它**（下一次 Begin 会重新问；服务端记账也照旧，
  避免每次组合结束都收/弹一次）。与 `_uiElementShown`（`IsShown` 的答案，元素存续期间的
  可见态、End 后归 FALSE、构造时也是 FALSE）**分开存**——拿后者当「宿主接管」会让普通宿主
  在激活时被误报成接管。
- `_uiLessThread`：`ActivateEx` 带 `TF_TMAE_UIELEMENTENABLEDONLY`。激活末尾即报 STATE，
  让候选窗**从第一个组合起**就不弹（否则要等首次 Begin 回 FALSE，先弹再收闪一帧）。
- 报 STATE 只在 flags 变化时（`_uiElementStateSent`），激活/停用都复位成 -1 强制重报。
- 宿主接管时的每次候选变化：`QUERY` → 快照 → 与上一份 diff 出 `TF_CLUIE_*` → `UpdateUIElement`。
  首次（Begin 回 FALSE 之后）全位置位。
- 宿主不接管时：getter 沿用占位数据（`GetCount=1`、"…"），**不拉快照**——保持 Chromium
  那条调度收益且不加键路径开销。
- `SetSelection(n)`：发 ACTION 后立刻再拉快照并 Update——同一条管道按序处理，拉到的就是
  新高亮。`Finalize/Abort` 的结果（上屏 / 清组合）经 push 管道回来，与鼠标点选同路，
  走既有的 `NotifyCandidatesVisibilityChanged(FALSE)` → `EndUIElement`。
- `SetPageIndex`：接受但不改切法（分页由 `ui.candidate.per_page` 决定；Weasel 同款）。
- **顺序：先开/更新组合，再注册候选元素**（`KeyEventSink.cpp` 的 `UpdateComposition` 分支）。
  IMM32 桥（经 `ImmGetCandidateList` 取候选的宿主，Dota 2 的 SDL 在非 UI-less 构建下就是）把
  `BeginUIElement` 映射成 `IMN_OPENCANDIDATE`，但只在组合已存在时才发；元素先于组合注册，
  桥就只发 `IMN_CHANGECANDIDATE`、永不发 `OPENCANDIDATE`，靠它才打开候选盒的宿主什么都不显示。
  本机 IMM32 测试宿主实测：改序前 0 次 OPENCANDIDATE。
- **候选导航也要通知**：翻页 / 上下移高亮时服务端只回 `Consumed`（组合串没变），
  `Consumed` 分支须在候选存在时调 `NotifyCandidatesVisibilityChanged(TRUE)`，否则宿主停在旧页、
  空格上屏的却是新页的词（本机 UI-less 测试宿主实测）。

### 4.3 快照的形状（`uielement_page_snapshot`）

**只带当页**：`GetCount` = 当页条数、页数恒 1、高亮为页内下标；`SetSelection` 的参数也按
页内下标解释。与 Weasel / 微软拼音同一形状。
第一版曾带「从 0 起至少 64 条」的前缀让宿主自己切页——Dota 2 这类自绘候选的宿主会把
`GetCount` 条**全部**画出来（微软五笔在 Dota 2 里「所有页一次显示、翻页崩游戏」正是这个
形状，微软拼音则正常，见 Microsoft Q&A #5631957），故收敛为当页。翻页/上下移高亮后 DLL
重拉，宿主看到的就是新的一页。文本走 `cand_s2t_text`（简繁显示与本地候选窗一致）。

### 4.4 服务端：按 pid 记账不弹窗（`handle_uielement.rs`）

- `uielement_host_pids: HashSet<pid>`；`notify_ui_update` 在 `hide_candidate_window` 守卫之后
  加一道 `ui_suppressed_by_host()`：命中则只发 `HideCandidates`、照常
  `reset_first_show`。**候选状态照常演进**——空格上屏、数字选词、翻页全部照旧，宿主画的
  正是这份状态。同一判据也压住**状态气泡**（`show_tip`）与**工具栏**（`notify_toolbar`）：
  规范要求 TIP 的任何 UI 都经 UIElementMgr 征得同意，这两个没有对应的 UIElement，只能不弹。
- 「当前在输入的进程」取 `focus_pid`：焦点/激活事件写它，**每个按键**也写它（bridge 按管道
  对端 pid 调 `note_key_source_pid`）。只靠 `active_compat.pid` 不够——游戏这类宿主常常没有
  可编辑 TSF 上下文，`focus_gained` 一次都不来。两者任一命中即压。不用「最近一条连接」：
  候选窗是全局的，接管是进程属性，游戏接管了切到记事本仍要弹。
- 状态翻转立刻 `notify_ui_update`：接管报告到达时窗已弹出（首次组合的应答先于报告）要收掉；
  撤销时弹回来。
- 清账：`handle_ime_deactivated`（token 高 32 位）与 `handle_client_connected`（新 DLL 实例会重报）。
  pid 复用残留最多让新进程首次候选被压一帧，且会被首次 `BeginUIElement` 的重报纠正。
- ACTION：`SetSelection` 按绝对下标落到 `current_page/selected_index`；`SetPage` 走
  `page_next/page_prev` 原语（它们负责动态扩展与末页放宽）；`Finalize` = `mouse_select(高亮)`；
  `Abort` = `cancel_session` + 推 `ClearComposition`。

### 4.5 独占全屏不弹窗（P2）

`is_foreground_fullscreen` 拆成 `foreground_fullscreen_kind() -> {None, D3dExclusive, Covering}`：
- 判据①（`SHQueryUserNotificationState` = `QUNS_RUNNING_D3D_FULL_SCREEN / PRESENTATION_MODE`）
  ⇒ `D3dExclusive`：**候选窗不弹**（`fullscreen_exclusive_cached`）。
- 判据②（矩形铺满）⇒ `Covering`：只影响工具栏（`fullscreen_cached`，沿用 `hide_in_fullscreen`）。
  无边框全屏下普通窗口叠加没有问题，候选窗照常。

探测仍在 `notify_toolbar_async` 的单飞线程里（焦点/激活事件触发），**不再**受
`ui.toolbar.hide_in_fullscreen` 门控——同一次探测要给两个缓存位刷值。独占态翻转时
`notify_ui_update` 一次，该收的收、该弹的弹。

**最后一道闸在 UI 线程**（`wind_keys::foreground::exclusive_fullscreen_recent`，300ms TTL）：
事件驱动的缓存在「游戏激活之后才切进独占全屏」时会过期，显示命令照发；于是 wind-ui 在
候选窗 / 状态气泡 / 工具栏每次**真正显示前**再问一次前台形态，独占全屏就改 hide。
Dota 2 实测第一版（只有事件缓存）一输入就弹窗把游戏卡死，这道闸就是为它加的。
探测函数因此从协调器搬到 wind-keys（wind-ui 不能依赖协调器）。

**不设配置键**：这不是偏好而是物理事实（独占全屏下别的进程的窗口盖不上去，弹出去只剩副作用），
按 config-design-rules R1「可由程序判定的走自动判定」。误判排查看 info 日志
`前台 D3D 独占全屏=…`。

### 4.6 系统目录部署（Trusted Mode 的解，见 §1.2.1）

TSF DLL 部署到 `%WINDIR%\System32\IME\WindInput[Dev]\`，x86 到
`%WINDIR%\SysWOW64\IME\WindInput[Dev]\`，**对系统副本** `regsvr32`。
`DllRegisterServer` 内的 `GetModuleFileName` 取到的就是系统副本路径，
`InprocServer32` 与 profile 图标路径**自然指向系统副本**，无需后处理。

**三条部署路径都要改**，否则出现「装的方式不同、游戏里能不能用也不同」：

| 路径 | 落点 | 开关 | 默认 |
|---|---|---|---|
| 安装器 | `wind-installer` `src/installer/ime.rs` | 清单 `[ime] system_subdir` | 本产品清单已置 `IME/WindInput`；字段空=就地注册 |
| 开发部署 | `scripts/dev.ps1` `Register-Tsf` | 无 | 固定走系统目录 |
| 便携模式 | `wind-portable` `src/registration.rs` | 便携根目录的 `system_deploy` 标记文件 | **就地注册** |

**★ 便携模式默认就地，与安装器相反。** 因为 `System32\IME\<AppName>\` 与
`HKLM\Software\<AppName>\InstallDir` 是**安装版与便携版共用的落点**——路径完全相同，
同机共存时后注册的一方会覆盖前者。安装版是机器上的唯一实例，占用它天经地义；
便携版可以有多份、还可能与安装版并存，默认不碰即零冲突。要在游戏里用便携版时，
用户显式建 `system_deploy` 标记开启，并接受与安装版争用同一份系统副本。

**⚠️ 由此派生一条删除守则**：便携版反注册时**只能动自己部署的那份系统副本**
（`owns_system_deployment`：`InstallDir` 指回本便携目录才算），无条件删会把安装版的
副本一并删掉、废掉那边的输入法。判据用「实际所有权」而非「当前开关值」——
用户可能注册后才关掉开关，那时标记没了副本还在，照开关判就会漏清。

**★ 安装目录回指 `HKLM\Software\<AppName>\InstallDir`。** DLL 搬进系统目录后，
`GetModuleFileName` 只能取到系统副本路径，**推不出安装目录**——而服务拉起
（`IPCClient::_StartService`）与便携标记检测都需要安装目录。三个部署方在 `regsvr32`
**之前**写该键（注册一完成宿主就可能加载 DLL，那时键还不在就会白走一次）；
读端 `wind_tsf/src/IPCClient.cpp` 的 `_ResolveAppBaseDir`，键缺失时回退到 DLL 自身目录，
兼容就地注册的存量部署。

**★ 便携模式的归属判据必须换。** 系统副本对同一变体的**所有实例路径完全相同**，
`is_registered` 再比 DLL 路径等于恒真。改由 `InstallDir` 判定所有权：谁最后注册，
它就指向谁的目录。`installed_conflict` 判断「注册来源是不是便携实例」同理——
系统副本旁边不可能有便携标记文件，得回到 `InstallDir` 指向的目录去找。

**⚠️ 32 位差异**：x64→`System32`、x86→`SysWOW64`，两个子目录都要建，且 x86 必须用
`SysWOW64\regsvr32.exe` 注册（注册项才落进 `WOW6432Node` 视图）。以上都**假定调用方是
64 位进程**（安装器只发 x64，dev.ps1 走 pwsh）——32 位进程访问 `System32` 会被 WOW64
文件系统重定向**静默**改写到 `SysWOW64`，x64 DLL 就装错了地方还不报错。
`dev.ps1` 的 `Get-TsfSystemDir` 为此显式断言而**不做自动纠正**：静默纠正会掩盖
「调用方本身跑错了架构」这个真问题。

**⚠️ 卸载/升级要收掉系统副本**：它不在安装目录里，安装器的 `DeleteInstallFiles` 够不着，
不显式删就永久滞留。删完文件顺带 `remove_dir` 收掉自建子目录（只删空目录，
另一架构副本还在时自然失败，正是需要的语义）；被宿主锁住删不掉的走「改名让路 + 排重启删」
并记进 reboot 账本，否则「需要重启」会漏判。

## 5. 非目标 / 备选（未做）

- **P3 组合串内联候选**（不走 UI-less 的独占全屏游戏）：独占全屏下用户看不到候选，只能盲打。
  一种常见的「游戏模式」做法是把当页候选并进组合串（`ni'hao 1.你好 2.拟好 …`），依赖宿主
  会显示组合串。它改变组合串语义（caret、宿主侧自动完成），须按进程/全局开关做，本轮未做。
- **`ITfIntegratableCandidateListUIElement`**（Win8+ 搜索框集成：`OnKeyDown` 路由、
  `ShowCandidateNumbers`）：SampleIME/Weasel 都实现了；本轮未做，宿主 QI 不到会按普通 UIElement 处理。
- **宿主不接管时也给真实数据**（读屏软件 NVDA 经 `ITfUIElementSink` 读候选）：会给所有宿主
  的键路径加一次往返，需要单独评估（例如只在检测到 UIA 客户端时开）。
- **状态泡 / 工具栏在 UI-less 线程下的抑制**：规范要求 TIP 的**任何** UI 都经 UIElementMgr；
  状态泡是独立窗口，本轮未接（`show_focus_status_if_enabled` 可复用 `uielement_host_draws()`）。

## 6. 涉及文件

- `wind-ipc/src/protocol.rs`、`codec.rs`：常量、`UiElementStatePayload/ActionPayload/Page`、编解码 + 往返测试。
- `wind-bridge/src/handler.rs`、`deferred.rs`、`server.rs`：trait 三方法、转发、分发 + 测试。
- `wind-coordinator/src/handle_uielement.rs`（新）、`coordinator.rs`（两个字段 + `notify_ui_update` 守卫）、
  `handle_menu.rs`（探测线程刷两个缓存位）、`coordinator/message_handler.rs`（trait impl + 清账）、
  `lib.rs`（`FullscreenKind`）。
- `wind_tsf/include/BinaryProtocol.h`、`IPCClient.h`、`src/IPCClient.cpp`（PAGE 解析）、
  `include/TextService.h`、`src/TextService.cpp`（UIElement 段重写、ActivateEx/Deactivate 接线）。

系统目录部署（§4.6），跨三仓：

- `wind_tsf/include/Globals.h`：`WIND_APP_NAME` / `WIND_APP_REGKEY` / `WIND_SERVICE_EXE`
  三个变体宏；`src/IPCClient.cpp` 的 `_ResolveAppBaseDir` + `_StartService` 改读注册表
  （顺带修掉「Dev 版读 release 的 `Software\WindInput` 键」这个既有变体缺陷）。
- `scripts/dev.ps1`：`Get-TsfSystemDir` / `Get-AppRegKey` / `Register-Tsf` / `Unregister-Tsf`；
  `Set-TomlKeysInSection`（清单 `[ime]` 段改稀疏替换，顺带修掉 `sweep_residue` 被整段
  替换静默丢弃的既有缺陷）。
- `config/app.toml`：`[ime] system_subdir`。
- `wind-installer`：`src/manifest.rs`（字段）、`src/installer/ime.rs`（部署/反注册/收目录、
  `system_subdir` 的路径穿越守卫）、`src/installer/registry.rs`（`set_install_dir`）、`app.toml`。
- `wind-portable`：`src/registration.rs`（系统副本部署、`InstallDir` 所有权凭据、
  `is_registered` 与 `installed_conflict` 的归属判据换轨）。

## 7. 验证

已做：wind-ipc / wind-bridge / wind-coordinator 单测（编解码往返、分发、按 pid 压窗、快照分页、
动作、独占全屏位）；DLL x64 Release 编译。

**本机可复现的 UI-less 宿主**：`dist/uiless_host.exe`（源码 `dist/uiless_host.cpp`，单文件
Win32 + msctf，CMake/MSVC 直接编）。它做三件事：以 `TF_TMAE_UIELEMENTENABLEDONLY` 激活线程、
advise `ITfUIElementSink` 回 `pbShow=FALSE`、把 `ITfCandidateListUIElement` 读到的一切打印到控制台。
两种模式：默认自带一个最小 `ITextStoreACP` 文本存储、按键经 `ITfKeystrokeMgr` 直接派给 TIP；
`--imm` 用 EDIT 控件走系统 IMM32 桥（清 `ISC_SHOWUIALL`，在 `IMN_*` 时 `ImmGetCandidateListW`），
模拟不带 UI-less 的老 SDL。`--auto --clsid {…} --profile {…}` 全自动敲 `nihao`、翻页、下移、空格。
⚠ TSF 只在线程拿到前台后才给 TIP 派焦点与按键，所以宿主会抢约 4 秒前台（用户此时敲的键会
进它的窗口，不会打进别的程序）；在独立桌面上跑过，`ActivateProfile` 失败（0x80004005），行不通。
真机用法：本机 `scripts\dev.ps1 pd1` 部署 worktree 构建后跑它，比对 Dota 2 要简单得多。

待真机（需要一个 UI-less 宿主）：
1. **SDL2 示例**（`SDL_HINT_IME_SHOW_UI=0`，或任何用 `TF_TMAE_UIELEMENTENABLEDONLY` 的程序）：
   DLL 日志应出现 `ActivateEx: TF_TMAE_UIELEMENTENABLEDONLY set`、`uielement state reported: flags=0x2/0x3`；
   服务端日志 `uielement: pid=… host_draws=true`；打字时**不弹**本地候选窗，宿主自绘列表内容
   与本地候选一致（含翻页、上下移高亮）；空格/数字上屏正常。
2. **ImeSharp**（`ryancheung/ImeSharp` 的 demo，`pbShow=FALSE`）：同上，另测其 `SetSelection/Finalize`。
3. **独占全屏游戏**（DXGI 独占，如设置里选「全屏」而非「无边框」的老游戏）：焦点进游戏后
   服务端日志 `前台 D3D 独占全屏=true`；打字不弹窗、游戏不被踢出全屏；Alt+Tab 回桌面后恢复。
4. **回归**：记事本 / Chromium / Word 键路径无新增 IPC（日志里不应出现 `UiElementPage`）；
   Ctrl+数字在 QQNT 里仍不双处理（占位 `GetCount=1` 保持不变）。
