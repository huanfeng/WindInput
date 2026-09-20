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


### 1.3 ⛔ 上屏后第一个退格失效：是**游戏吞的**，白名单 `product_id` 决定（2026-09-13 定案）

**症状**：Dota 2 / CS2 里中文上屏后，紧接着的第一下退格无效，第二下起正常。只在游戏里出现，
记事本等宿主无此现象；**回车在同一位置正常**。

#### 判据：一次日志就能定案

`wind_tsf.dota2.*.log`（`level=trace`）里，那一键只有两行：

```
OnTestKeyDown: wParam=0x08
compat.context_status focusSession=? flags=0x80000000 readonly=0 loading=0
```

此后**什么都没有**。三条缺失各自独立成立：

| 缺什么 | 说明 |
|---|---|
| 无 `compat.key phase=test_down` | `pfEaten` 恒 FALSE——**所有**吃键分支都必然紧跟一行 `_LogKeyDecision` |
| 无 `Sending key event` | 一个字节都没发给引擎，core 侧根本不知道按过退格 |
| 无 `phase=down` | 游戏在我们返回 FALSE 后**根本不调 `OnKeyDown`**，键全程在它手里 |

**退格两次、回车一次，三者日志逐字相同**，而回车表现正常 ⇒ 差异 100% 在游戏侧。
这也是 `session_key` 分支的设计内行为：上屏后 `composing=0 candidates=0`，
`_HasInputSession()` 为假，退格本就该还给宿主。

#### ⛔ 已各花一轮真机证伪的方向（勿重试）

- **改上屏形态**：组合区 `SetText` 改为 `SetText("")` + `EndComposition` + `InsertTextAtSelection`
  （即本函数注释里那个被废弃的历史实现）——无效。
- **实现 `ITfReadingInformationUIElement`**（`21c4e33e`）——无效。Dota 确实 QI 它（一次会话 50 次）
  并读 `GetString`（17 次），但**只在组合活跃期间**读、上屏后一次都不读，空串没机会告诉它。
  ★ 副产品：开启后**我们的嵌入编码会显示在游戏的专门小栏里**，与不实现时的显示方式不同
  ——这是真实 UI 差异，要不要采用属产品选择，代码可从 `21c4e33e` 原样捞回。
- **拆 `UI_LESS_THREAD` / `HOST_DRAWS` 判据**（`wind-ipc/src/protocol.rs`）——无效。
  不在白名单时 Dota 照样回 `show=0`，声称接管却不画。
- **用「宿主读不读候选数据」区分会不会画**——无效。两种状态都完整读完
  （`GetUpdatedFlags` → `GetPageIndex` → `GetString`×N），不在白名单时读得**更多**。

#### ★ 触发条件精确到白名单的一个字段

`imemanager.dll` 的 `.rdata` 偏移 **`0x33E60`** 起是 **145 条 × 24 字节**的结构体数组：

```c
struct ImeEntry { const char* name; const char* canonical; uint64_t product_id; };
```

`product_id` 决定走哪条 IME 兼容分支，**退格坏不坏由它定**：

| `product_id` | `canonical` | 上屏后首个退格 |
|---|---|---|
| `0x210000` | Sogou Pinyin | ✅ 正常 |
| `0x110000` / `0x810000` / `0x910000` / `0xA10000` / `0xC10000` | Google / QQPinyin / NianQing / WuBi / JJ Pinyin | ❌ 吞 |
| `0x410000` | **Unknown**（兜底类，第 90 条 `拼音输入法` = 现出厂值） | ❌ 吞 |
| `0x030000` 等微软自带系 | 郑码 / 双拼 / 全拼… | ❌ 吞 |

**全表逐个真机试过，只有 Sogou 那条分支不吞退格**，而它对应的四个名字全是搜狗品牌，
不能拿来发版 ⇒ **换一个内置别名解决不了退格**。

出厂值 2026-09-13 由 `中文 (简体) - 郑码`（第 89 条，`0x30000`）改为 **`拼音输入法`**
（第 90 条，`0x410000`）。退格行为两条分支相同，换的是另外三件事：新值**不含空格、
括号、连字符**，而「括号写成全角 / 空格多一个少一个」正是这类别名最常见且最不可察觉的
翻车方式；它是通用类别名而非具体码表方案名（本输入法并不提供郑码方案，旧值对用户是
误导）；它也是全表最短的条目，手抄出错的面最小。

> **2026-09-13 起名字改为可配置**（`system.dota2_compat_name`，设置页「游戏兼容 → 登记名称」）。
> 开放的理由正是上表：这 145 条的行为按 `product_id` 分了好几种，而我们只在
> Dota 2 / CS2 上逐条验过。内置第二、第三个名字等于替用户选一种我们无法穷举验证的取舍——
> 别的游戏、别的分支可能是另一套结果，交给用户按自己玩的那个游戏实测更诚实。
> （出厂值同日一并换了，见上一段；两件事是一起做的。）
>
> ⚠️ 这不改变上面的结论：**我们自己能挑的名字里没有不吞退格的**。可配置只是把「换名字试试」
> 这个动作从改代码降成改设置，不是解决了退格。

⚠️ 两个采样陷阱：`拼音输入法` 看着像中立通用名，其 `canonical` 实为 `Unknown`
（这不影响它做出厂值——它照样在表内、照样命中，只是走兜底分支）；
`微软五笔` 的**实际注册名是 `Microsoft Wubi`**（英文，同在表里、同为 `0xA10000`），
所以「微软五笔也坏」是**命中白名单**的数据点，不是未命中。

#### 全表（含中日文的条目，2026-09-13 从 Dota 2 的 `imemanager.dll` 原样提取）

⛔ **这是仓里唯一一份白名单副本。** 写文档示例、改出厂值、回答「某某名字在不在表里」
都以它为准 —— 之前没有这份副本，示例名字无从核对，本文档差一点就教用户去抄一个
根本不存在的条目（`中文 (简体) - 五笔`，凭印象编的，表里没有）。
`name` 列可直接抄进设置页的「登记名称」。同一 `product_id` 下的若干名字走同一条兼容分支。

| `product_id` | `canonical` | 第几条 · `name`（可直接抄进「登记名称」）|
|---|---|---|
| `0x1` | Phonetic | `106` `中文(繁体) - 注音` <br> `125` `中文 (繁體) - 注音` |
| `0x2` | New Phonetic | `107` `中文(繁体) - 新注音` <br> `126` `中文 (繁體) - 新注音` <br> `138` `微軟注音` <br> `139` `微软注音` |
| `0x3` | ChangJie | `105` `中文(繁体) - 仓颉` <br> `123` `中文 (繁體) - 倉頡` |
| `0x4` | New ChangJie | `115` `中文(繁体) - 新仓颉` <br> `124` `中文 (繁體) - 新倉頡` <br> `142` `微軟倉頡` <br> `143` `微软仓颉` |
| `0x5` | DaYi 6.0 | `116` `繁体中文大易(版本 6.0)` <br> `129` `中文繁體大易 (version 6.0)` |
| `0x6` | Array 6.0 | `117` `繁体中文行列(版本 6.0)` <br> `130` `中文繁體行列 (version 6.0)` |
| `0x7` | Quick | `22` `Microsoft 速成` <br> `23` `微软速成` <br> `108` `中文(繁体) - 速成` <br> `127` `中文 (繁體) - 速成` |
| `0x8` | New Quick | `109` `中文(繁体) - 新速成` <br> `128` `中文 (繁體) - 新速成` |
| `0x10000` | Quan Pin 6.0 | `112` `简体中文双拼(版本 6.0)` <br> `134` `中文簡體雙拼 (version 6.0)` |
| `0x20000` | Shuang Pin | `88` `中文 (简体) - 双拼` <br> `111` `简体中文全拼(版本 6.0)` <br> `132` `中文簡體全拼 (version 6.0)` |
| `0x30000` | Zheng Ma | `89` `中文 (简体) - 郑码` <br> `110` `简体中文郑码(版本 6.0` <br> `113` `简体中文郑码(版本 6.0)` <br> `133` `中文簡體鄭碼 (version 6.0)` |
| `0x40000` | Microsoft Pinyin IME 3.0 | `72` `中文(简体) - 微软拼音输入法` <br> `75` `中文 (简体) - 美式键盘` <br> `131` `中文 (簡體) - 微軟拼音輸入法` |
| `0x50000` | MSPinyin IME | `69` `微软拼音` <br> `70` `微軟拼音` |
| `0x70000` | Microsoft Pinyin IME 2007 | `114` `微软拼音输入法 2007` |
| `0x110000` | Google Pinyin | `78` `中文 (简体) - 谷歌拼音输入法` <br> `79` `谷歌拼音输入法` <br> `81` `谷歌拼音输入法 2.0` <br> `83` `谷歌拼音输入法 2` |
| `0x210000` | Sogou Pinyin | `80` `中文 (简体) - 搜狗拼音输入法` <br> `84` `搜狗拼音输入法` <br> `87` `中文(简体) - 搜狗拼音输入法` |
| `0x410000` | Unknown | `90` `拼音输入法` |
| `0x810000` | QQPinyin | `91` `中文 - QQ拼音输入法` <br> `92` `中文 - QQ五笔输入法` |
| `0x910000` | NianQing | `93` `中文 (简体) - 念青繁體五筆 2.03` |
| `0xA10000` | WuBi86 | `94` `微软王码五笔86版` <br> `96` `微软王码五笔输入法 98版` <br> `99` `微软五笔` <br> `100` `微軟五筆` |
| `0xC10000` | JJ Pinyin | `102` `中文 (简体) - 加加输入法5.0` |

共 53 条含中日文条目（全表 145 条，其余为纯 ASCII 的日/韩/西文输入法名）。

⚠️ 第 `110` 条 `简体中文郑码(版本 6.0` **确实少一个右括号**，是游戏那侧的笔误，照抄才命中。

提取方法：解析 PE 段表建立 VA↔文件偏移映射（`ImageBase=0x180000000`），
从**文件偏移 `0x33E60`** 起按 24 字节步进读出全表，每条的前两个 `QWORD` 是指向
UTF-8 窄串的 VA。⚠️ `0x33E60` 是**文件偏移**，它落在 `.data`（raw `0x033400`）而不是
`.rdata`——按「`.rdata` 内偏移」去解一条都解不出来。

#### 现状：`dota2_compat` 是个二选一

开 ＝ 有候选窗、上屏后首个退格失效；关 ＝ 退格正常、无候选窗。设置页应把这个权衡说清。
三条出路（A 已实施其「说清权衡」的一半，B/D 未实施）：

- **A 维持现状**，把权衡写进设置说明——**已做**：设置页的说明写明了代价，且名字本身
  开放给用户改（见上），让「想试别的分支」的人自己试，而不是我们替所有人定；
- **B 关 compat 时我们自己画候选**：⚠️ 有 §4.5 那个黑屏卡死的复发风险——独占判据逐键抖动，
  正是「`host_draws` 无条件压窗」才断开那个反馈环；
- **D 开 compat 时我们接管退格**（吃键 + `ReplacePrecedingChars` 代删）：游戏收不到退格后它的
  composition 缓存不清 ⇒ **所有**退格都要我们代劳，语义变了，且是 Dota 专用 hack。

#### ⛔ 别名跨重装能不能留住：判据只能是我们自己的记录值

2026-09-13 在实测机上用 **dev 变体**（另一对 CLSID/profile GUID，不碰正在用的注册）
逐步验过 `regsvr32 /u` → `regsvr32` 这一轮：

| 步骤 | `TIP\{CLSID}` | `Description` | 我们键下的 `Dota2CompatAlias` |
|---|---|---|---|
| 注册后 | 在 | `清风输入法 (开发版)` | 无 |
| 模拟用户开兼容 | 在 | `搜狗拼音输入法` | `搜狗拼音输入法` |
| **`regsvr32 /u` 后** | **没了** | **没了** | 还在 |
| 重新注册 | 在 | ← 取决于判据 | 还在 |

`ITfInputProcessorProfiles::Unregister(CLSID)` 会把整个 TIP 键连同 `Description` 删掉，
而安装/升级走的正是「先反注册、再注册」（`wind-installer` 的 `UnregisterOldCom` →
`RegisterCom`；`dev.ps1` 的 `Unregister-Tsf` → `Register-Tsf`）。

⇒ **「读当前 Description 来判断要不要保留别名」在唯一会触发它的场景里必然落空。**
`_ProfileNameToRegister()` 最初那版（以及更早只认硬编码别名的那版）都栽在这里，
且毫无痕迹——症状是「升级后语言栏变回清风输入法、游戏里没候选，而设置页仍显示已开启」。
判据改为以 `HKLM\Software\<app>\Dota2CompatAlias` 为准后同一实验已通过。

### 1.4 ⚠️ 传统 IMM32 宿主：多出一个候选框，还停在第一帧（2026-09-13 定案）

现象（论坛 65 楼 253 帖，台服新枫之谷 `MapleStory.exe`，0.121.0 起出现）：
游戏聊天框里**两个候选框**，上屏的字取自我们自己的那个；游戏画的那个只显示第一个码的
候选，之后再按键也不更新。用户原话：「上個版本不會出現那個藍色候選框」。

#### 证据链（用户日志 `wind_tsf.MapleStory.10508.log` + `wind_input.1.log`，五笔86）

按 `T` → `Y` → `K` 一次组合：

| 时刻 | 事件 | 宿主拿到的 |
|---|---|---|
| 23.781 | 按 `T` → `BeginUIElement ok id=0 **show=1**` | 快照 = `t` 的候选 `和 禾 長 季 麼 知 秀 行 生` |
| 23.930 | 按 `Y` 后宿主读 `GetString(0..8)` | 和 禾 長 季 麼 知 秀 行 生 |
| 24.783 | 按 `K` 后宿主又读 | **同上，一字未变** |

同一时刻核心日志 `UpdateCandidates` 画的是 `ty` 的候选（`时 野 里 蛙 旰 昧 旱 蝻 暑`）。
`GetUpdatedFlags` 恒回 `0x3F`（初值）也是佐证——真刷新过会算出差异位。

#### 两个根因

1. **快照只在 `BeginUIElement` 时取过一次。** `NotifyCandidatesVisibilityChanged` 的第三
   分支在宿主未声明接管时只发 `UpdateUIElement`、不刷快照；而 getter 的判据是
   `_UiElementUseSnapshot() = _uiHostDraws || 快照非空`，Begin 前的预取让快照恒非空
   ⇒ 整个组合期间如实交付**第一帧**。

2. **那个框本不该出现。** 它不是游戏自绘、也不是真 UI-less（真 UI-less 会回
   `pbShow=FALSE`，它回的是 TRUE），而是 **CUAS 的 IMM32 桥**把我们的 TSF 候选列表桥接成
   IMM32 候选列表后，由宿主或 `DefWindowProc` 画出的**旧版系统候选窗**。触发它的是
   `80314c8a`（首发 v0.121.0）那条换序——「先开组合再注册元素」让桥开始发
   `IMN_OPENCANDIDATE`。⚠ 那条换序当初是为 Dota 2 做的，而 §1.1 后来已把 Dota 2 那条路
   **整条证伪**（身份闸门，对我们零次 `ImmGetCandidateList`）：它没救到目标宿主，却给所有
   走 CUAS 桥的传统宿主凭空开了一个重复的框。

#### 定案：读了就算它在画（⛔ 2026-09-15 已改成 opt-in，见下文「推断被证伪」）

> **本小节记的是 2026-09-13 的原定案，读时请连同下文的修正一起看。**
> 判据本身（观测「宿主读没读」）保留，**但它不再默认收窗**——要收的宿主得在 compat 里
> 写 `host_drawn_candidates = true`。下面描述的「命中即收窗」现在只对 opt-in 的宿主成立。

判据是 **`ITfCandidateListUIElement::GetString` 被调用**（真把候选文本取走），
sticky 到本次激活结束，经 `UIELEMENT_FLAG_HOST_READS` 报给服务端。命中后：

- 服务端收起我们自己的候选窗（`ui_suppressed_by_host` 判据三）——宿主画的那个贴着它
  自己的输入框（位置由它的 `ImmSetCandidateWindow` 决定），比我们在游戏里靠 caret 猜的准；
  这类宿主的 caret 本来就取不到（同一份日志里恒为退化矩形 `w=1 h=0`，我们的窗被摆到
  `(-1201,250)`，另一个显示器上）。
- 快照改为**记脏 + 懒刷**：候选变了只置 `_uiSnapshotDirty`，宿主真来读 getter 时才补拉。
  不读的宿主一次 IPC 都不多；会读的宿主从第一次读起就拿到当前候选。
- `BeginUIElement` 回 `pbShow=TRUE` 时也补一次 `UpdateUIElement`（规范说可不调）——
  **为的是在首键就把会读的宿主逼出来**：实测 IMM32 桥在 Begin 之后静默 1.3s 不读，
  第二键的 Update 一到立刻读满 9 条。不补这一下，读取闩要等到第二键才合上，而我们的
  候选窗在首显闸门（25~600ms）里早弹出来了，用户看到的就是「先两个框、再收掉一个」。

#### ⛔ 2026-09-15 修正：推断被证伪，改成 opt-in

上面那条「读了就算它在画」立案时自陈**没有反证样本**（见本节末尾的 ⚠）。反证在靶机上
一次到齐，该判据作为**默认行为**就此作废。

##### 反证

同一台靶机、同一轮实测，三个宿主把候选串整串读走（`GetString(0..4)`）却**都不画候选窗**：

| 宿主 | 读取次数 | 画候选窗 |
|---|---|---|
| `Notepad.exe`（Win11 WinUI3 新记事本） | 288 / 96 / 56（三次会话） | 否 |
| `Illustrator.exe` | 80 | 否 |
| `EverEdit.exe` | 8 | 否 |

这不是新行为：同机**旧版 DLL**（判据上线前）的日志里记事本同样读了 96 次，只是当时不
据此收窗。⇒ **读候选串是这些宿主的常态，不是自绘的标志**。

发现经过本身也值得记一笔：该判据 2026-09-14 提交，而靶机 `System32\IME\` 下的 TSF DLL
停在 09-12（部署脚本只推安装目录、没同步系统副本，宿主按注册表 `InprocServer32` 加载的
是系统副本）。补上同步的当天它第一次真正生效，记事本立刻变成「一个候选框都没有」。

##### 定案：语义从 opt-out 翻成 opt-in

`host_drawn_candidates` 不再是「单向关闭开关」，而是**单向开启**：

```toml
[[apps]]
process = "MapleStory.exe"     # 或 "*" 套到全局
host_drawn_candidates = true   # 读走候选串时让出绘制权
```

- 不写 / `false` ⇒ 读了也**照弹我们的候选框**（默认）
- `true` ⇒ 读了就判自绘、收我们的窗

新枫之谷那份修复由出厂 `data/compat.toml` 里的一行 `MapleStory.exe` 保住，不丢。

##### 为什么默认值该站在「不收窗」这一侧

**两种误判的代价不对称**：

| 判错方向 | 用户看到 | 能不能用 | 自救 |
|---|---|---|---|
| 判成「宿主在画」，实际没画 | 两个候选框一个都没有 | **完全不能用** | 得先知道有这么个 compat 字段 |
| 判成「宿主没画」，实际在画 | 多出一个框 | 字照打，能用 | 配一行即可 |

一个明确标着「是推断不是事实」的判据，不该把不可用那一侧设成默认。

`pbShow=FALSE` 那条（宿主的**声明**）不受本次修正影响：它是事实不是推断，照旧默认生效、
也照旧不给关。两者分两位上报、分两张 pid 账，正是为了能只改推断这一半的默认。

按 `config-design-rules` R1「可由程序判定 ⇒ 自动判定 + compat 覆盖，不加用户键」——本次
改的是自动判定的默认值，没有新增用户键。

通配 `"*"` 保留，用途转为反方向（某类宿主普遍需要收窗时一行开到全局）；它仍只被这一条
判据的回落查表认（`HOST_DRAWN_WILDCARD`），**没有**做成 `AppCompat::get_rule` 的通用通配
——那会让一条规则把全部字段套到每个应用头上。

⚠ 查规则时必须用**命中的那个 pid**（`current_pid_in` 回 `Option<u32>`），不能另取一次
`active_process_name()`：后者只认 `active_compat.pid`，而游戏宿主常常没有可编辑 TSF
上下文、`focus_gained` 一次都不来，`active_compat` 会停在**上一个进程**上（既有测试
`key_source_pid_alone_matches_host_draws` 钉的就是这个分岔）。查错了 pid ⇒ 用户给游戏配的
opt-in 静默失效，新枫之谷又变回两个框。回归测试 `the_compat_rule_follows_the_key_source_pid`
（变异检验：查名换成 `active_process_name()` ⇒ 它和 `the_key_source_pid_wins_when_both_pids_are_readers`
一起红）。

##### 没有动的东西

**那条换序（`80314c8a`「先开组合再注册元素」）没有撤销**，尽管本节下文已经指出它才是
重复框的源头、且它当初要救的 Dota 2 已被 §1.1 整条证伪。原因是撤销它需要新枫之谷环境
复验「IMM32 桥不再发 `IMN_OPENCANDIDATE`」，手上没有该环境；而 opt-in 化不依赖任何未验
假设就能让普通宿主恢复可用。**撤销换序仍是更彻底的修法，待有环境时评估**——那之后这条
推断判据连同 `MapleStory.exe` 的出厂规则都可能一并移除。

#### 两条刻意留着的缺口

**① `_CaretQuerySuppressed()` 不跟读取闩走**，只看宿主的声明。后果是：推断生效、core 已
收窗时，DLL 这边仍照常向宿主探 caret——收了窗却没省掉收窗本该省掉的开销。

不改的理由有两条。其一，把闩算进去，「用 compat 关掉了推断」的用户拿到的不是「坐标陈旧
的窗」而是**没有任何坐标来源的窗**：`_CaretQuerySuppressed()` 拦的是三处
（`RequestFocusCaretAsync` / `SendCaretPositionUpdate` / `OnLayoutChange`），而兜底 timer
就设在 `SendCaretPositionUpdate` 里面，提前返回连它一起取消——而那个覆盖正是误判时的
唯一退路。其二，同样的缺口对**判据二（D3D 独占全屏收窗）本来就存在**：DLL 侧对全屏
一无所知（`wind_tsf/` 全仓搜 `fullscreen`/`exclusive` 零命中），那也是 core 侧的决定、
一样传不到 DLL ⇒ 不是本次新开的一类问题。

⚠️ **但这一条不是「以后再说」，是带触发条件的跟进项。** 它重新暴露的具体故障是 §1.1 那条
「`OnLayoutChange` burst + 50ms debounce + 同步 `GetTextExt` 把渲染线程拖死」；此前它只在
一个**已宣告不可达**的宿主（Dota 2）上可及，判据三把它变成在一个**已确认的、且是游戏的**
现场宿主上可及。新枫之谷日志里 caret 是退化矩形而非冻死，所以未被证实。
**触发条件：一旦收到任何「`uielement_host_reads` 命中的全屏宿主出现卡顿/冻结」的反馈，
立刻做 core→DLL 的「本进程已被收窗」推送**，一次把两条判据都覆盖掉。

**② 合闩那一次读取的同步 IPC 发生在 msctf 的 sink 派发栈内**（`UpdateUIElement` → 宿主
`GetString` → `_EnsureUiSnapshotFresh`），最坏按住宿主 UI 线程 `READ_TIMEOUT_MS`(1500ms)。

频率上界（2026-09-14 更正，初版写的「每次激活至多一次」**不成立**）：
- 正常路径确实每次激活至多一次——这次调用会在 `GetString` 的**成功出口**合上闩，此后
  第三分支走急刷、dirty 恒为 FALSE，再也走不到；
- 但闩只在成功出口合。`GetString` 前面两个早退（补拉回空/失败 → 占位；`uIndex` 越界 →
  `E_INVALIDARG`）都跳过合闩，于是下一键 dirty 又置真、再付一次。真实上界是
  **「快照查询持续失败或回空时，每键一次」**，由 IPC 熔断（`_ShouldAttemptOperation` /
  `_RecordFailure`）兜住，撑不到持续每键 1500ms。

⛔ 不要为此调小超时：读超时与 `_RecordFailure`/`Disconnect`/熔断是同一套，调小换来的是
负载高时误断 IPC + 熔断 3 秒，比顿一下更糟。上面那条「失败时每键一次」恰恰是**不该调小**
的又一条理由——熔断才是这条路的正确闸门。

#### 为什么不回退 `80314c8a` 的换序

重复框的触发源是那条「先开组合再注册元素」的换序，而它当初为 Dota 2 而做、§1.1 已把
Dota 2 整条证伪 ⇒ 它现在的已知收益是零、已知代价是给所有走 CUAS 桥的传统宿主开了个
重复框。回退它能让命中推断的宿主少一大截，从而让「读了就是在画」这条**尚无反证样本**的
推断承载的重量小得多。

不回退的理由：换序本身对**真 UI-less 宿主**（`pbShow=FALSE`）是必需的——§4.2 记的本机
IMM32 测试宿主实测「改序前 0 次 `OPENCANDIDATE`」，靠 `OPENCANDIDATE` 才打开候选盒的
宿主回退后会什么都不显示。两类宿主共用同一条注册路径，回退是把 A 的修复换成 B 的回归。
判据三解决的是「谁来画」，换序解决的是「桥发不发 `OPENCANDIDATE`」，两者不能互相替代。

⚠️ 这个取舍的前提是「真 UI-less 宿主确实需要那条换序」——证据只有一个本机测试宿主，
没有现场样本。若将来证明真 UI-less 宿主不依赖它，回退换序比维持判据三更划算，届时应当
重新称量。

#### ~~未验证的一环~~ → 已验证，且结论是**反的**（2026-09-15 结案）

原文如下，保留以存档：

> **「只有真要画的宿主才会来读候选串」至今没有反证样本。** 那份日志里 7 个宿主只有
> MapleStory 读过（Excel / WebView2 / Beanfun / AweSun / DwarfAxe 全是 0 次），但另外 6 个
> 当时**都没有出现过候选**，构不成负对照。要补这个对照，需要在一个普通桌面宿主里真打一次
> 字、看日志有没有 `UIElement host read: GetString`。

补对照的办法说对了，结论则相反：在普通桌面宿主里真打字之后，**`Notepad` / `Illustrator` /
`EverEdit` 三个都出现了 `UIElement host read: GetString`，而三个都不画候选窗**。该假设作为
普遍规律不成立，判据已改成 opt-in，详见上文「⛔ 2026-09-15 修正」。

⇒ 方法论上值得记一笔：一条写着「没有反证样本」的假设被直接投入默认行为，而拿到反证只
花了在任意一个普通宿主里打一次字的工夫。**「暂时没有反例」不等于「已经验过」**；缺负对照
时，默认值应当选故障代价小的那一侧，把另一侧留给 opt-in。

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
| `CMD_UIELEMENT_STATE 0x0217` | DLL→核心 | 异步 | `pid u32 + flags u32`；bit0 宿主接管、bit1 UI-less 线程、bit2 宿主读走了候选串（§1.4） |
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
- `_uiHostReadsCandidates`（§1.4）= 宿主没声明接管、却调了 `GetString` 把候选文本取走。
  sticky 到 `Deactivate`（与 `_uiHostDraws` 一并清——pid 会复用）。
  `_UiElementHostDraws() = _uiHostDraws || _uiHostReadsCandidates`。
  判定点刻意选 `GetString` 而不是 `GetCount`/`GetUpdatedFlags`：后两者 msctf 自己也会问，
  不足以说明有人要画；**取走文本**才是。占位分支（无快照时回 "…"）不打闩。
  ⚠ 报 STATE 时 bit0 只带 `_uiHostDraws`，读取闩单独占 bit2——合并成一位，核心侧就没法
  只关掉推断的那一半。
- 宿主接管时的每次候选变化：`QUERY` → 快照 → 与上一份 diff 出 `TF_CLUIE_*` → `UpdateUIElement`。
  首次（Begin 之后）全位置位。
- 宿主未声明接管时：候选变化只置 `_uiSnapshotDirty` + `UpdateUIElement`，**不拉快照**；
  宿主真来读时由 `_EnsureUiSnapshotFresh(contentRead)` 补拉一次（成功与否都清脏位——
  失败还留着会让后续每个 getter 都重试一次同步 IPC，把宿主 UI 线程拖死）。
  ⚠ 闩未合上时**只有 `GetString` 会触发补拉**：`GetCount` 是 msctf 自己也会问的
  （它据此判断候选 UI「有没有意义」，Chromium 的 IME-first 调度靠这个），在那里无条件
  补拉就等于给每个宿主的每一次按键都加一次同步 IPC——正是本次要避免的代价。
  ⚠️ **取元信息那 5 个 getter 里的补拉当前不可达**（2026-09-14 复核）：脏位只在
  「未认定在画」那一支被置起，而读取闩只在 `GetString` 里合上、合闩之前必先补拉并清脏
  ⇒「脏 + 闩已合」进不来。留着是防御性的，真值表单测 `TestUnreachableCells` 把这条
  不变量钉住。⛔ 别再把它写成「闩合上之后所有 getter 都补拉、保证一次读取序列内同源」
  ——同源实际来自第三分支的急刷，而**恰恰在合闩那一次读取序列里不成立**：那一帧
  `GetCount` 走占位分支答 1，宿主据此只读 index 0，会画出只有一条候选的框（内容是对的、
  条数是缩的），下一键即恢复。Begin 之后那次补发的 `UpdateUIElement` 正是为了让宿主在
  **首键**（dirty=FALSE、条数真实）就来读、把这一帧也消掉，但宿主对紧跟 Begin 的
  Update 会不会同拍响应，日志里没有样本 ⇒ 未证。
  ⛔ 别改成无条件每键拉取：那是给**每个**宿主的每一次按键都加一次宿主 UI 线程上的同步
  IPC（读超时 1500ms），代价落在所有人身上，而真正需要它的只有会读的那一小撮。
  无快照且宿主不接管时 getter 仍回占位（`GetCount=1`、"…"），保住 Chromium 那条调度收益。
- `BeginUIElement` 之后**无论 `pbShow` 回什么都补一次 `UpdateUIElement`**。规范只要求回
  FALSE 时必须调；回 TRUE 时仍调是为了在**首键**就把会读的宿主逼出来（IMM32 桥在 Begin
  之后静默不读，要等下一次 Update 才来，见 §1.4）。不读的宿主不会因此拉快照，键路径零额外 IPC。
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
重拉，宿主看到的就是新的一页。文本走 `cand_convert_text`（简繁显示与本地候选窗一致）。

### 4.4 服务端：按 pid 记账不弹窗（`handle_uielement.rs`）

- **两张 pid 账**：`uielement_host_pids`（宿主**声明**接管，无覆盖）与
  `uielement_reader_pids`（宿主**读走过**候选串；**默认不据此收窗**，compat 写
  `host_drawn_candidates = true` 才生效，见 §1.4 的 opt-in 修正）。分开存是因为对它们的
  态度不同：一个是事实、一个是推断，而只有推断那条的默认值需要让位给「判错了也还能用」。
  取 pid 的口径共用 `current_pid_in()`，免得一张改了另一张没改。
- `notify_ui_update` 在 `hide_candidate_window` 守卫之后
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
  **两张账一起清**——声明账的残留会被首次 `BeginUIElement` 的重报纠正，读取账不会：
  pid 复用后新宿主会一上来就被判成自绘，而它自己的 `GetString` 不一定会发生，没人来纠正。
- ACTION：`SetSelection` 按绝对下标落到 `current_page/selected_index`；`SetPage` 走
  `page_next/page_prev` 原语（它们负责动态扩展与末页放宽）；`Finalize` = `mouse_select(高亮)`；
  `Abort` = `cancel_session` + 推 `ClearComposition`。

#### 压制态一律强制嵌入编码（`app_inline`）

`ui_suppressed_by_host()` 命中时 `preedit_uses_placeholder()` 直接回 false
（`coordinator/message_handler.rs`），即**不管用户配的是哪种 `preedit_display`，都按
`app_inline` 走**：真编码原样写进宿主组合区。

不这么做的后果是编码**两条路全断**：

| 编码的出口 | 非 `app_inline` 时 | 压制态下 |
|---|---|---|
| 宿主组合区（TSF composition） | 换成占位空格 `COMPOSITION_PLACEHOLDER` | 还是一个空格 |
| `UiCommand::UpdateCandidates::preedit` → 候选窗编码栏 | 真编码走这条 | `notify_ui_update` 发完 `HideCandidates` 就 return，**根本不下发** |
| 交给宿主自绘的 `UiElementPage` | — | 结构里**没有**编码串字段（只有 items/selected/page_size/current_page） |

于是 UI-less 游戏里一个编码字都看不见，而用户只是在设置里把编码栏挪到了候选窗顶部。
占位存在的唯一理由是「别和候选窗的编码栏重复显示」，压制态下候选窗压根不画，那条理由
不存在，占位就是纯粹的信息丢失。

- 判据收口在 `ui_suppressed_by_host`，**三个来源一视同仁**（声明接管 / D3D 独占全屏 /
  compat opt-in 的读取者）：它们在「编码丢了」这件事上没有分别。
- `host_render` 与 `hide_candidate_window` 刻意**不在**此列。前者候选窗照画、只是换个地方画
  （数据走 SHM 到宿主进程内的 DLL band 窗口），编码栏还在，强制嵌入会变成两处重复显示；
  后者是用户自己关的窗，属于另一件事。
- **不设配置键**：同 §4.5 的理由，「候选窗这一刻画不画得出来」是可由程序判定的物理事实，
  按 config-design-rules R1 走自动判定。真出现判错的宿主，该加的是 compat 里的 per-app
  `preedit_display` 覆盖，不是一个全局开关。
- 护栏在 `handle_uielement.rs::a_suppressed_host_gets_the_real_code_inline`，断言落在**按键
  出口**（`handle_key_event_policed` 返回的组合区文本）而不是判据函数的布尔值上——判据返回
  什么不算数，被那个 if 读到才算数。两个变异都验过：删早退 ⇒ 三条正向红且复现原 bug
  （拿到 `" "`）；判据改成恒 false ⇒ 开头的反向对照红。

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
  `handle_menu.rs`（探测线程刷两个缓存位）、`coordinator/message_handler.rs`（trait impl + 清账 +
  `preedit_uses_placeholder` 里的压制态强制嵌入编码，见 §4.4 末）、`lib.rs`（`FullscreenKind`）。
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
