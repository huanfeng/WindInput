# 智能符号（Smart Symbol）宿主兼容性问题记录

本文记录 2026-07 排查智能符号功能（`input.symbol.smart_mode`，连按同一中文标点转英文）在不同宿主下的兼容性问题、已修复项，以及一个尚未彻底解决、决定搁置的遗留问题。

## 背景：两种实现方案

`input.symbol.smart_method` 枚举（`wind-config/src/config.rs`）：

- `HoldComposition`（默认）：press1 把中文符号放进 TSF 组合态（预上屏），press2 直接替换组合提交英文；超时无 press2 则自动提交中文。整个过程只用 TSF 组合态 API，不发送任何合成按键。
- `DeleteReplace`：press1 直接提交中文符号（正常上屏），press2 时删除光标前 N 个字符、替换为英文。实现在 `wind_tsf/src/TextService.cpp` 的 `CTextService::ReplacePrecedingChars`。

下面「已修复的问题」到「遗留问题（本次搁置）」几节，讲的是 `DeleteReplace` 方案在 `ReplacePrecedingChars` 里暴露的一系列宿主兼容性问题；最后的「HoldComposition 方案」一节讲默认方案自己的问题族，两者互不相干。

## 已修复的问题

### 1. Office 下 500ms 后符号重复上屏

`HoldComposition` 超时收口（`OnHoldTimerExpired` → `CommitText`）是从 `SetTimer` 回调发起的 `TF_ES_SYNC` 请求，不在真实按键的同步上下文里。Word 对这类请求的校验比其它宿主严格，会出现"`DoEditSession` 内部其实已经执行完毕，但外层 `RequestEditSession` 的 `hr`/`hrSession` 报告失败"的情况。旧代码要求 `hr`、`hrSession`、`GetSuccess()` 三者都成功才算数，误判失败后继续走 `SendInput` 兜底，在已经正确写入的文字后面又打一遍。

**修复**：`CommitText`、`ReplacePrecedingChars` 改为只信 `GetSuccess()`——它只在 `DoEditSession` 内部真正执行完 `SetText`/`EndComposition` 后才置 `TRUE`，是文档是否已被修改的唯一可信信号（`TF_ES_SYNC` 请求下 `DoEditSession` 必然在 `RequestEditSession` 返回前同步跑完，读取顺序没有竞争）。

### 2. 部分终端/微信下合成按键被自己的钩子二次处理

`ReplacePrecedingChars`、`CommitText`、`InsertText` 的 `SendInput` 兜底注入的退格/字符键，此前没有任何"这是我自己刚合成的按键"标记，会被自己的 `KeyEventSink::OnTestKeyDown` 钩子当成真实用户输入又处理一遍。

**修复**：复用 auto-pair 已有的 `_PushSkipKey`/`_TryConsumeSkipKey` 跳过表机制，新增公开方法 `CKeyEventSink::MarkSyntheticKey(vk)`，三处 `SendInput` 兜底注入前都调用它标记（退格用 `VK_BACK`，Unicode 注入的字符用 `VK_PACKET`）。同时把 `MAX_SKIP_KEYS` 从 16 提到 64（`CommitText`/`InsertText` 可能一次标记一整段文本）。

### 3. 微信 / Windows Terminal 下智能符号完全不触发

真正的阻塞点，比替换执行细节更靠前：`prev_char`（`TextService.cpp` 的 `OnEndEdit` 现读文档得到，光标前一字符）在这类宿主里经常读取失败，恒为 0。服务端（`wind-coordinator/src/handle_punct.rs`）判定 press2 的条件要求 `prev_char` 与武装串末位字符相等，`prev_char=0` 时永远不匹配，于是永远判定"不是 press2"，把第二次按键当全新 press1 处理——服务端根本没有下发过 `ReplaceBackward`。

**修复**：`prev_char == 0` 视为"宿主读不回文档"而非"确定不匹配"，退回只信服务端自己的武装状态（armed + key + timeout，与文档内容无关）判定 press2。改了 `handle_punct.rs` 里 `DeleteReplace` 和 `HoldComposition→fallback` 两个分支。

### 4. 小键盘数字后智能标点不生效（备用 prevChar 通路漏了 numpad VK）

第 3 条给读不回文档的宿主留了备用通路 `_lastPassthroughDigit`，但它的记录判据两处都写成
`wParam >= '0' && wParam <= '9'`（VK `0x30`-`0x39`），**只覆盖主键盘**。小键盘数字是
`VK_NUMPAD0`-`VK_NUMPAD9`（`0x60`-`0x69`），不命中；而 `OnTestKeyDown` 那处记录点带
`else` 分支，于是小键盘数字不但记不上，还会把先前主键盘攒下的值**清零**。

必经性：`ClassifyInputKey` 把 numpad 数字归为 `HotkeyType::Number`，中文模式下 `Number`
只在「有 input session」或「全角」时才吃键（`KeyEventSink.cpp` 的 `session_select_or_page` /
`chinese_fullwidth_number` 两个分支）。「打完数字再打标点」时缓冲为空、通常也非全角，
数字必然透传，必然落到记录点——不是偶发路径。

**症状为什么像是随机**：能读回文档的宿主（记事本/浏览器/Office）走主路径
`ConsumeCachedPrevChar`，与本缺口无关，一切正常；只有 EverEdit 这类读不回的宿主暴露，
于是表现为「同一个功能换个程序就时灵时不灵」。

**修复**：抽出 `_DigitCharFromVk(vk, modifiers)` 作为唯一判据，主键盘与小键盘都认（同文件的
`_IsHoldReplayKey` 一直是两种都列的，这里纯属遗漏），两个记录点共用；返回 0 统一表示
「这一键不产出数字」，正好对应清零语义。顺带排除 `Shift+主键盘数字`——它产出的是 `!@#` 而
非数字。NumLock 关闭、或 NumLock 开着按 Shift+小键盘时，系统发的已是 `VK_END`/方向键，
语义本就不是数字，天然不命中。

**一并补上的同族缺口**：数字**经引擎上屏**时（全角数字、小键盘 `direct` 的「顶屏候选再追加
数字」、候选文本本身带数字）`pfEaten` 为真且响应不是 `PassThrough`，两个按键侧记录点都不
覆盖，读不回文档的宿主里这些场景此前是全丢的。新增 `_TrackCommittedTextForSmartPunct`，
在 `CommitText`（非 `restartComposition`）与 `InsertTextWithCursor` 两个「文本落地且不留组合」
的响应分支按上屏文本末位更新——末位是 ASCII 数字则记，否则清零。`restartComposition` 分支
刻意不记：它提交后立刻又起了组合，光标前是组合内容而非 `response.text` 末位。全角数字
（U+FF10-FF19）也刻意不记，服务端只认 ASCII `0x30`-`0x39`。

注意小键盘小数点 `VK_DECIMAL` **不在此列**——它被归为 `Number`、中文模式下直接
透传出半角 `.`，压根不进引擎，结果本来就是对的。

### 4b. 同族缺口：消费判据抄了一份 `smart_list`（出厂默认的冒号就失效）

第 4 条当时留下一句「未修」：备用通路的**消费**判据硬编码
`keyCode == VK_OEM_PERIOD || VK_OEM_COMMA`（消费与清零两处）。这不是「只差自定义符号」的
小缺口——出厂默认 `input.punct.smart_list = ".,:"`（`data/config.toml`）**本来就带冒号**，
所以读不回文档的宿主里，冒号从设计上就拿不到备用 `prevChar`，且会落进 `else` 分支把已记
的数字**清零**，连带毁掉紧随其后的句号（`1:.` 两个都不生效）。

当时判断「要修得干净需要把 `smart_list` 推给 DLL 并做字符→VK 映射，属于新增配置通道」。
**这个判断是错的，方向反了。** 正确的分工不是让 DLL 也知道 `smart_list`，而是让 DLL
**别再知道**它：

- DLL 侧只上报**事实**——「光标前一个字符是什么」。消费判据放宽成「这一键是不是标点键」
  （`ClassifyInputKey(...) == HotkeyType::Punctuation`，比 `IsPunctuationKey` 多覆盖
  Shift+主键盘数字产出的 `!` `@` `?` 等）。
- 服务端 `wind-punct::is_smart_punct_after_digit` 持有全部**策略**：`smart_after_digit`
  总开关 + `smart_list` 成员判定 + `0x30..=0x39` 数字判定。多报一个不在 `smart_list` 里的
  标点键的 `prevChar` 没有副作用，服务端自己判 false。

零新增配置通道、零协议变更，改动只是删掉那份过时的策略快照（`VK_OEM_PERIOD || VK_OEM_COMMA`
正是 `smart_list` 早期默认值 `".,"` 的快照，后来列表加了 `:` 而 DLL 那份没跟上）。

**★ 判断一个条件该放边界哪一侧，问「这个信息另一侧看得见吗」**：「光标前是数字」只有 DLL
看得见（数字键透传出去，根本不进 IPC），所以「哪些键**产出数字**」必须留在 DLL；而「这个
符号参不参与智能标点」服务端自己就有配置，DLL 判就是抄——抄了就会漂移。同一条边界上的两个
判据，方向可以是相反的。

**验证锚点**：备用通路命中时打 `smart_punct_digit_fallback`（DEBUG）。主路径能读回时该行
不出现——两条通路症状相同、成因不同，只有这条日志分得开。它同时是「新 DLL 是否真的编进/
部署上」的自证串（`tr -d '\000' < wind_tsf_dev.dll | grep -ao smart_punct_digit_fallback`）。

### 5. macOS 上数字后智能标点从未生效（`prevChar` 恒 0）

macOS 侧 `KeyHandler.encodeKeyEvent` 一直写死 `prevChar: 0`（注释 "M2.1 暂不取 caret 前字符"），
服务端 `is_smart_punct_after_digit` 的最后一道判据是 `(0x30..=0x39).contains(&prev_char)`，
于是该功能在 macOS 上**从未接通过**——不是回归，是未实装。

**决定只做备用通路，不做「现读文档」主路径。** IMKit 有 `selectedRange` + `attributedSubstring`
（`InputController.selectedClientText` 已在用），但那是跨进程同步调用：项目里已经为它的开销
把取选中文本挪出了 `activateServer`（实测 1191/4643 个采样点），且宿主支持度参差。只跟踪
「我们自己送进文档的东西」换来的是**所有 app 里行为一致**，代价是光标被外部移动（鼠标点击、
⌘V）时会漂——判据因此一律取保守侧：拿不准就清零，回落「按中文标点出」。

实现：`SmartPunctDigitTracker`（`wind_macos/Sources/WindInputKit/IPC/`），语义逐条对齐 Windows
的 `_DigitCharFromVk` / `_TrackCommittedTextForSmartPunct`，含本文第 4、4b 两条的教训（小键盘
VK 必须认；消费点不许抄 `smart_list` 白名单）。三个记录点，缺一不可：

| 记录点 | 位置 | 语义 |
|---|---|---|
| 透传的按键 | `InputController.passThroughToHost` | 宿主拿这一键去改文档，按「这一键产出什么」推断 |
| 上屏的文本 | `BridgeResponseRouter.insertCommitted` | 经引擎写进文档的末位字符 |
| marked text | `BridgeResponseRouter.applyMarkedText` | 组合/待定符号也摆在光标前（光标不在串尾则清零） |

还有三条**改了文档却不经 `insertText`** 的路，各自单独清账——`insertCommitted` 那个收口
拦不住它们，评审时一并补齐：`moveCursor`（智能跳过合成右方向键）、`replaceBackward` 的
空文本（`ime.undo_commit` 的纯删除）、合成按键四路（`key.tap/hold/release/seq` 经
`PushResponder.invalidateDigitTracking`）。判据都是「拿不准就清零」。

⚠️ `digitChar` 的修饰键守卫比 Windows 的 `_DigitCharFromVk` **多挡 ⌘/⌃/⌥**，不是抄漏了：
TSF 的 `OnTestKeyDown` 在记录点之前就滤掉了宿主快捷键，而 IMKit 把每个 keyDown 都交给
`handle`，⌘2 切标签页照样走到透传记录点。不挡就会记进一个根本没进文档的幻影数字——
比漏记更糟，漏记只是少触发一次，这个是**多**触发。

第三个记录点是**智能符号自己的**必需项，不只是为数字后智能：press1 走 HoldComposition 时符号
只进 marked、不 `insertText`，漏记的话 tracker 残留数字 → press2 时服务端拿 `prev_char` 与武装串
末位比对失配 → 第二次按键被当成全新 press1，连按替换静默失效。第 3 条那个 `prev_char == 0`
容错正是靠这里清零才够得着。

「删掉数字再打标点应回落中文标点」是这套记账的自然结果：退格透传 = 非数字键 = 清零，
不为它写特判。

## 已知但决定不修的问题：TSF 报告成功但实际渲染未生效（Tabby / 微信）

实测发现 Tabby（Electron/Chromium 内核终端）、微信（Qt 内核）这两个宿主自制的 TSFTextStore，对 `CReplaceBackwardEditSession` 的 `ShiftStart`+`SetText` 会**全程报告成功**（`hr`、`hrSession`、`GetSuccess()` 皆 `S_OK`），但实际画面上旧符号没删掉、新符号又插入了一份。同一段代码在 Notepad/Office 等原生编辑控件里结果是对的——不是我们这边 range 算错，是宿主自己的 TSFTextStore 内部模型跟它真实渲染的内容对不上，单靠更严格检查 TSF 返回码无法识别。

一度尝试过"默认全局改用真实合成按键（不信任 TSF 成功与否）"，但这会在 EverEdit 之类宿主上撞到下面这个新问题，且用户按住 Shift 连续输入多个符号时无法排队处理（见下）。**最终决定：`ReplacePrecedingChars` 默认仍然优先走 TSF 同步 range 替换**（`kTryTsfRangeReplace = true`，`TextService.cpp`），Tabby/微信这个问题暂不处理；如果以后确认必须解决，思路是按宿主进程名特判，只在已知问题宿主上切到合成按键路径，而不是全局切换。

## 遗留问题（本次搁置）：EverEdit 下 Shift 类标点删改失败

### 现象

EverEdit 编辑器里，`DeleteReplace` 的 TSF range 替换**稳定真实失败**（`hrSession=0x80004005` / `E_FAIL`，每次必现，不是间歇性谎报），代码正确地回退到 `SendInput`（退格 × count + Unicode 注入新文本，已加 `MarkSyntheticKey`）。

- 不带 Shift 的标点（如句号"。"→"."）：SendInput 兜底表现正常。
- 需要 Shift 组合键才能打出的标点（如 Shift+"-" 打出中文破折号"——"）：删改失败，旧符号没删掉，新符号又插入了一份（表现为重复上屏）。

日志实证（同一 EverEdit 会话）：

```
mods=0x0（句号，无 Shift）→ TSF failed → SendInput fallback → 结果正确
mods=0x11（Shift + "-"）    → TSF failed → SendInput fallback → 结果错误
```

### 根因

`SendInput` 注入 `VK_BACK` 时，Windows 会把当前**物理**按住的修饰键状态和注入的按键叠加——只要 Shift 还按着，宿主收到的实际上是"Shift+Backspace"而不是普通退格。EverEdit 把这个组合键解读成了别的操作（不是删除单个字符），于是旧符号删不掉；新文本走 `KEYEVENTF_UNICODE` 直接注入 Unicode 码点，不受修饰键影响，正常插入——两者叠加就是"重复上屏"。

### 已排除的两种修法

1. **注入前临时松开 Shift，退格后再恢复 Shift**：这是最直觉的修法，但代码里 `_SimulatePairKey` 相关注释早就记录过并放弃了这条路——松开/恢复 Shift 会让 OS 产生新的 Shift 按下/抬起事件，可能污染中英文切换的判定状态机（`_pendingKeyUpKey` 等）。本次复核确认了具体原因：`OnTestKeyDown`/`OnTestKeyUp` 会检查"自生成按键跳过表"（`_TryConsumeSkipKey`），但**真正维护中英切换状态机的 `OnKeyDown`/`OnKeyUp` 完全不检查这张表**，即使把合成的 Shift 按键标记为跳过也拦不住状态机被污染。要根治得把 skip 检查也接入 `OnKeyDown`/`OnKeyUp`——这两个函数是所有按键处理的公共入口，改动面和回归风险都不小，需要仔细回归热键、中英切换等功能。

2. **延后到修饰键释放后再执行替换**（仿照 `_SimulatePairKey` 的 `_pendingPairAction` 延迟队列）：已实现过一版又撤销。问题是用户可能按住 Shift 连续输入多个符号（例如连按多次 Shift+"-"），每次替换都相对"当前光标位置"操作；如果排队等到 Shift 最终松开才批量执行，前面几次替换执行时的光标位置假设已经过时，容易执行到错误的位置——这不是简单排队能解决的时序问题，且用户明确反馈了会"卡住"。

### 后续方向（未实施，供下次参考）

- 方向 A：把 skip 检查接入 `OnKeyDown`/`OnKeyUp`，让"松开/恢复修饰键"技术真正安全，根治所有 Shift 类合成按键场景（不限于智能符号）。工作量最大，但最通用。
- 方向 B：检测到"TSF range 替换真失败 + 修饰键按住"时，直接放弃这次智能符号转换（保留中文符号原样，不报错也不重复），把功能收窄成"无 Shift 才支持"，改动小但功能有缺口。
- 方向 C：调研 EverEdit 具体把 Shift+Backspace 解读成了什么操作，看是否有更精确的绕过方式（例如换一种删除机制，如 `WM_CHAR` 直接注入退格字符 `\b` 而非 `VK_BACK` 虚拟键，规避虚拟键+修饰键叠加的问题——未验证是否可行）。

## HoldComposition 方案：吃键门控与「吃了再吐」问题族

以下是默认方案 `HoldComposition` 自己的问题族，与上面 `DeleteReplace` 那几节无关。

### 共同机制

press1 之后符号进入 TSF 组合态，`_pTextService->HasActiveComposition()` 为真，于是 `_HasInputSession()` 为真。这个判据是 `OnTestKeyDown` 决定吃不吃键的总闸，结果是**hold 预览期间几乎所有功能键都会被吃下**——即便用户主观上并不认为自己在输入中（他只是刚打了个标点）。

被吃下的键随后到 `OnKeyDown`，服务端因为缓冲为空多半回 `PassThrough`，`pfEaten` 变 `FALSE`。两阶段决定不一致，就是本仓反复遇到的「吃了再吐」翻转：

- 记事本（走 IMM32 兼容层）、Chromium 系宿主会补发被吐回的键，用户无感；
- EverEdit 这类严格按 TSF 语义实现的宿主不补发，**键直接丢失**。

判断某个键有没有踩这个坑，看的是「会不会被吃键门控捕获 + 服务端会不会回 PassThrough」，**与按键语义无关**——这是本族问题的统一判据。

### 已修：Enter / 空格 / 退格 / 方向键 / 数字键等（`6b7e87a`）

最早暴露的是回车：hold 期间按 Enter 只上屏符号、不换行。除了上述翻转，还叠加了第二层原因——**组合态活着时回车是宿主的通用语义「确认输入」而非换行**，任何 IME 都一样，所以即便宿主补发也仍然不换行。

试过两条路都被真机推翻：

1. 在 `OnTestKeyDown` 里提前 Flush 收口再放行。写入确实成功、选区也 Collapse 到末尾了，EverEdit 实测却打出 `\n。`——宿主处理「TSF 文档变更」和「WM_KEYDOWN」是两条独立路径，不保证前者先落地，我们没有任何 TSF 手段能强制它先消化写入。
2. 只改判据不主动收口，指望宿主在改文档前自己 finalize 组合。实测回车可行、**空格不会终止组合**、符号要等定时器到期，行为不一致。

最终方案是把两个动作都收进我们控制的顺序：**吃掉原键（与 `OnTestKeyDown` 的决定一致，无翻转）→ 在 `OnKeyDown` 这个合法编辑上下文里收口 → `SendInput` 重放**。宿主先看到收口后的文档，再看到一个与组合无关的普通按键。键表见 `KeyEventSink.h` 的 `_IsHoldReplayKey`。

### 已修：全角等上屏路径吞掉待定符号（`5a37b96`）

`CommitText` 走的是**组合 range 的 `SetText`**，而 range 里此刻显示的正是那个待定的中文符号。此前一律 `CancelHoldTimer()` 丢弃，符号随即被静默覆盖——表现为全角下「。」+ 空格只剩全角空格。

难点在于 `CommitText` 自己分不清该覆盖还是该保留：智能符号 press2 需要覆盖（「。」→「.」），全角空格需要保留，而两者在 IPC 载荷上完全同构。解法是让语义显式化而非让接收端推理——新增 `COMMIT_FLAG_REPLACING_HELD`（`CommitText` flags bit3）+ `KeyAction::CommitReplacingHeld`，**默认改为追加语义**（`AbsorbHeldIntoPrefix`），只有 press2 显式声明替换。

默认取追加而不是给全角路径单独打补丁，是因为 hold 期间可能触发提交的路径远不止一处（全角空格/数字、临时英文、各独占模式出字、中英切换时的待定文本提交），把安全的一侧设为默认，新增路径自动正确。顺带修好了中英切换（`SystemModeSwitch` / `Ctrl+Space`）时 hold 符号被丢弃的同类问题。

### 搁置：Ctrl/Alt 组合（Ctrl+S 等宿主快捷键）

**症状**：hold 预览期间按 Ctrl+S，EverEdit 这类严格宿主收不到，保存不生效；记事本/Chromium 正常。

**实测确认的路径**（`Coordinator` 探测，非推断）：

```
OnTestKeyDown   hasInputSession（hold 组合活跃）→ 吃键 pfEaten=TRUE
                （日志理由 ctrl_alt_cleanup，可据此串日志）
OnKeyDown       → 服务端 → PassThrough          ← 实测：Ctrl+S / Ctrl+C 均如此
                → PassThrough 分支已调 FlushHoldCompositionIfActive()   ← 符号其实收口了
                → 重放分支：_IsHoldReplayKey('S') = FALSE，不重放
                → pfEaten=FALSE                                        ← 翻转，与 Enter 同构
```

复现探测用的是 headless `Coordinator`：构造 `smart_mode=true` + `smart_method=HoldComposition`，先送逗号键（`0xBC`）确认返回 `HoldComposition`，再送带 `MOD_CTRL` 的 `0x53`（S）看返回值。这条探测随手可重做，别再凭 `_isComposing` 的值推断。

注意曾有一个错误判断被写进代码注释又被推翻：以为 Ctrl 组合走 `isCtrlAltCleanup` 分支、响应为 `Ack`、键被**吃掉**。实际服务端回 `PassThrough`，`pfEaten` 为假，`isCtrlAltCleanup && *pfEaten` 那段压根不执行；符号收口也已经由 `PassThrough` 分支完成。所以这不是「被吃掉」，就是普通的「吃了再吐」。

**修法**（同构，未实施）：重放分支的条件放宽一个修饰键判断即可——

```cpp
if (holdActiveBeforeResponse && !(*pfEaten)
    && (_IsHoldReplayKey(wParam) || (modifiers & (KEYMOD_CTRL | KEYMOD_ALT))))
```

重放时物理修饰键仍按着，宿主 `GetKeyState` 能还原 Ctrl+S 语义；Alt 组合同理会正常生成 `WM_SYSKEYDOWN`。

**为什么搁置**：触及面小——只在「hold 预览的 500ms 窗口内」+「严格 TSF 宿主」两个条件同时成立时才发生，且符号本身不会丢（`PassThrough` 分支已收口），丢的只是那一次快捷键。等有用户实际反馈再动。

**一并留个提醒**：普通输入会话（有候选）下的 Ctrl 组合走的是另一条路——服务端返回非 `PassThrough` 时会进 `isCtrlAltCleanup` 那段，末尾同样有一句 `*pfEaten = FALSE`，理论上是**同一个翻转**。那条路覆盖 Ctrl+C/V/Z/S 全部高频快捷键，改动面比 hold 大得多。**该路径的服务端响应尚未实测**，要动之前必须先用同样的探测方法确认，不要沿用本节结论。

## 后续扩展：两条新增触发通路（2026-07-28）

原本只有一条通路：中文标点模式下 press1 出中文 → press2 换英文。现在多了两条。它们**复用同一套 press2 执行机制**（同样的 `ReplaceBackward` / `CommitReplacingHeld`，同样的 `prev_char==0` 容错），故本文上面所有宿主兼容性结论对它们一并适用。

### 1. 反向：数字后智能标点

`3.` 这类场景 press1 照旧出英文 `.`（数字后语义不变），但不再是终点——时限内再按一次换回中文 `。`。方向记在 `SmartSymbolArm::reverse`，press2 只是把「取哪一列产物」翻过来，删改机制一字未动。

此前 `smart_symbol_arm_str` 遇数字后智能直接 `return None` 拒绝武装，于是数字后想打中文标点只能去关掉「数字后智能」总开关，粒度粗到没法用。

### 2. 模式进入键的二次按下

`;`（快捷输入）/ `` ` ``（临时拼音）/ `\`（特殊模式）这类被模式占用的符号键，在模式内空缓冲时二次按下会上屏中文标点并退出模式（`handle_mode.rs` / `handle_temp.rs` / `handle_special.rs` 三处），此刻顺带武装（`arm_smart_symbol_after_commit`），第三次按下即换英文形。仍受 `symbol.smart_chars` 参与集合门控。

两处必须知道的约束：

- **这条通路恒走删改路径**。符号是经 `CommitText` 真上屏的，没有组合态可以覆盖，因此**即使用户选了 `HoldComposition` 方案，press2 也只能是 `ReplaceBackward`**（走 `smart_symbol_press2` 里 `held_text.is_none()` 那条降级分支）。也就是说上文 EverEdit / Tabby / 微信那一系列删改问题对它同样成立——而选 `HoldComposition` 的用户本来正是为了绕开删改。默认的三个进入键都不是 Shift 组合键，故 EverEdit 那条「Shift 叠加致 `VK_BACK` 变 Shift+Backspace」的坑不触发；用户把引导键配成 Shift 组合符号（如 `~`）时才会撞上。
- **press2 的拦截点在 `handle_lifecycle.rs::try_activate_mode` 开头，必须早于模式激活链**。空闲态按下这些键会被模式进入抢走，走不到标点分支的智能符号判定——武装了也白武装。该拦截三重收窄（仅空闲态、仅被模式占用的键、仅判 press2 不武装），普通标点的路径完全不变。

### 3. 英文标点状态（`symbol.english_punct_mode`）

中文输入模式 + 工具栏把标点切成英文（`chinese_punct=false`）时，连按同键把英文标点换成中文。标点键在这条路上本来就进引擎（主标点分支），故是纯 Rust 改动，无宿主风险。

### 4. 英文输入模式（`symbol.english_mode`）

整个输入法切英文（`chinese_mode=false`）时同样可用。**这条通路要先把键从 DLL 手里要回来**：英文半角下 DLL 默认透传标点键，引擎收不到。做法是把 `symbol.english_chars` 并入 `CONFIG_KEY_CUSTOM_EN_PUNCT` 推送（`ConfigBundle::build` 合并两个来源），DLL 侧判据 `_IsCustomEnglishPunctKey` 是数据驱动的字符集查表，**C++ 一行没改**。

- **铁律仍是「C++ 吃键集 ⊆ Rust 出字集」**。合并后集合同时是推送内容与 `handle_english_custom_punct` 的接手判据（都读 `rt().custom_en_punct_chars`），同源即不漂移；集合内没配英半自定义的键会出原样 ASCII，与透传等价，故并入安全。改动其中一侧时必须同时改另一侧。
- **副作用：英文本地配对对这些键让位给 core**（`KeyEventSink.cpp` 的 `_IsCustomEnglishPunctKey` 同时是配对让位判据）。core 侧有自己的配对处理，但 DLL 的跳出栈是空的 → 若用户把配对符放进 `english_chars`，那对括号的 Tab 跳出会失效（既有已知限制的扩大面）。默认集合 `.,?!:;` 不含配对符。
- 两个开关独立于中文侧、也彼此独立，且全部默认关——英文态默认保持纯净。

### 三种上下文的共同约束

`SmartSymbolArm::mode_snapshot` 记下 press1 当时的 `(chinese_mode, chinese_punct)`，press2 要求两者都没变。三种上下文各有独立开关与独立产物列，press1 后用户切了模式再按同键，必须当成全新 press1——否则会在新上下文里按旧方向删掉文档里的字。这条守卫替换了原先写死的 `state.chinese_punct` 判定。

另有一个易错点：press1 的武装串必须用 `press1_committed_str` 而非 `compute_punct_str_pure`。后者的英文半角列**刻意不查自定义**（它的语义是「press2 的替换目标，须保持原样英文」），而武装串要等于真正插进文档的东西——用户把 `;` 的英半列配成 `#` 时，press1 上屏 `#` 而武装串若还是 `;`，press2 的 `prev_char` 比对永远失配、功能静默失效。三条反向通路的 press1 都落在英文列，都必须走前者。

## 相关代码位置

- `wind_macos/Sources/WindInputKit/IPC/SmartPunctDigitTracker.swift`：macOS 侧 `prevChar` 记账（无主路径，见第 5 条）；喂食点在 `BridgeResponseRouter.insertCommitted` / `applyMarkedText` 与 `InputController.passThroughToHost`。
- `wind_input/crates/wind-coordinator/src/handle_punct.rs`：`try_smart_symbol_replace`（press1 武装 + press2 分发）、`smart_symbol_press2`（press2 状态机、`prev_char` 判定、方向分歧点）、`smart_symbol_arm_str`（中文输入模式的两种上下文 + 方向判定）、`english_mode_smart_symbol`（英文输入模式通路）、`arm_smart_symbol_after_commit`（模式进入键武装）、`press1_committed_str`（武装串的唯一真相源）
- `wind_input/crates/wind-coordinator/src/handle_lifecycle.rs`：`try_activate_mode` 开头的 press2 拦截、`is_any_mode_trigger`
- `wind_input/crates/wind-coordinator/src/coordinator.rs`：`ConfigBundle::build`（吃键集合并）、`push_custom_en_punct_config`（推送时机：配置热重载广播 + 新客户端连接）
- `wind_input/crates/wind-punct/src/lib.rs`：`english_participates`（按源字符判定的理由）、`english_smart_source_chars`
- `wind_input/crates/wind-coordinator/tests/smart_symbol.rs`：四条通路的端到端锁
- `wind_tsf/src/TextService.cpp`：`CommitText`、`ReplacePrecedingChars`（含 `kTryTsfRangeReplace` 开关）、`CReplaceBackwardEditSession`（含诊断用 readback 日志）
- `wind_tsf/src/KeyEventSink.cpp` / `include/KeyEventSink.h`：`MarkSyntheticKey`、`_PushSkipKey`/`_TryConsumeSkipKey`、`_SimulatePairKey`（Shift 合成按键相关历史教训）

HoldComposition 方案：

- `wind_tsf/src/TextService.cpp`：`HoldComposition`、`AbsorbHeldIntoPrefix`（并入语义）、`CancelHoldTimer`（丢弃语义）、`OnHoldTimerExpired`、`FlushHoldCompositionIfActive`
- `wind_tsf/src/KeyEventSink.cpp` / `include/KeyEventSink.h`：`_HasInputSession`（吃键总闸）、`_IsHoldReplayKey`、`_ReplayKeyToHost`，以及 `OnKeyDown` 里 `holdActiveBeforeResponse` 的采样点与重放分支
- `COMMIT_FLAG_REPLACING_HELD`：`wind_tsf/include/BinaryProtocol.h`、`wind_input/crates/wind-ipc/src/protocol.rs`、`codec.rs` 的 `encode_commit_text_replacing_held`
