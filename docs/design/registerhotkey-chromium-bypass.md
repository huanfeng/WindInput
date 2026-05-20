# Win32 RegisterHotKey 绕开 Chromium 宿主加速键双处理

> **当前作用范围**：仅 **Ctrl+0..9（Pin）与 Ctrl+Shift+0..9（Delete）** 候选热键。
> `Ctrl+=`（AddWord）曾尝试走本通路但**已回退到 OnKeyDown 路径**，原因见下文
> "失败尝试" 表与"已知限制"章节。

## 问题背景

部分基于 Chromium / Electron 的应用（典型代表 **QQNT / QQ**）实现 TSF IME 集成时，**不遵循"IME 吃了就停"的标准约定**。表现：

- IME 在 `OnTestKeyDown` / `OnKeyDown` 设置 `pfEaten = TRUE`
- 但宿主的加速键处理（如 `Ctrl+1..9` 切换 Tab/会话）**与 IME 处理并行触发**
- 结果：用户按 `Ctrl+2` 既让 IME 置顶第二个候选，**又**让 QQ 切到第二个会话

这与标准 Chromium 文档矛盾（Chromium 设计是 IME 接受后跳过加速键），是 QQNT 自身 Electron 实现的偏差，IME 侧无法通过 TSF 接口改正。

Pin/Delete 是**纯命令型操作**——按下即执行，不需要后续按键或 TSF composition
状态参与，所以适合用 RegisterHotKey 这种"键被 OS 提前消费、TSF 完全感知不到"
的机制。AddWord (Ctrl+=) 则是**会话型操作**——按下后还要接收 ESC/↑/↓/Enter
继续路由，必须依赖 TSF composition / EditSession，强行放在 RegisterHotKey 通路
会带来一系列严重问题（见下）。

## 调研历程

### 失败尝试（保留作为反面教材）

| 路径 | 结果 |
|------|------|
| `OnTestKeyDown` + `OnKeyDown` 都返回 `pfEaten=TRUE` | Edge 尊重，QQNT 不尊重 |
| `ITfKeystrokeMgr::PreserveKey` 注册业务热键 | `TF_E_INVALIDKEY`；且无法做条件吃键 |
| `ITfThreadFocusSink` 注册 | 行为无变化 |
| `ITfUIElement` + `ITfCandidateListUIElement` + `BeginUIElement` | 行为无变化 |
| `ITfFunctionProvider` 通过 `ITfSourceSingle::AdviseSingleSink` 注册 | 行为无变化 |
| `OnKeyDown` 内同步 `RequestEditSession(TF_ES_SYNC)` | 死锁 + QQ 崩溃 |
| 把 `Ctrl+=` AddWord 也接入 RegisterHotKey + DispatchHotkey | **回退**：DispatchHotkey 从 WM_HOTKEY WndProc 进入，**不在任何 TSF 事件回调栈内**，Chromium 类宿主同步 `RequestEditSession` 被拒，导致 QQ/WPS/浏览器 caret 拿不到、TSF composition 起不来、ESC/↑/↓ 无法回到 IME；加上跨进程 `ERROR_HOTKEY_ALREADY_REGISTERED (1409)` 互抢，反而引发"另一个进程拦截了热键"的异常状态 |

### 突破口

静态反汇编**第三方输入法**的 `ime` TSF DLL，发现它 import 了 `RegisterHotKey` / `UnregisterHotKey`，并在内部注册了 **38 个**系统级热键：

- `Ctrl+0..9`（10 个）
- `Ctrl+F1..F9`（9 个）
- `Ctrl+Shift+0..9`（10 个）
- `Ctrl+Shift+F1..F9`（9 个）

`fsModifiers` 为 `MOD_CONTROL` (2) 或 `MOD_CONTROL | MOD_SHIFT` (6)。

## 为什么 `RegisterHotKey` 能解决问题

Win32 `RegisterHotKey` 在 Windows 的键盘消息派发路径上**先于 `WM_KEYDOWN`** 工作：

```
[硬件按键]
   ↓
[Raw Input]
   ↓
[键盘布局处理]
   ↓
★ RegisterHotKey 匹配 → 命中 → 发 WM_HOTKEY 到注册窗口 → 该键不再产生 WM_KEYDOWN
   ↓ （未命中）
[WM_KEYDOWN 派发到焦点窗口]
   ↓
[Chromium TranslateAccelerator → 加速键]
   ↓
[TSF IME 处理]
```

一旦命中 `RegisterHotKey`：
- 焦点应用（QQ）的**消息泵根本收不到 `WM_KEYDOWN`**
- Chromium 的加速键调度连参与机会都没有
- **没有"双处理"问题**

这是 Win32 系统级保证，与 TSF / Chromium 实现无关。

## 与 `WH_KEYBOARD_LL` 的关键区别

`WH_KEYBOARD_LL` 是低级键盘钩子，常被误读为"全局钩子"。两者实际差异：

| 维度 | `WH_KEYBOARD_LL` | `RegisterHotKey` |
|------|-------------------|------------------|
| 注册范围 | 全局（拦截所有窗口的所有键） | 进程内/线程内（仅指定键组合） |
| 安全敏感度 | 高 - EDR/杀软常拦 | 极低 - 标准 IME 用法 |
| 钩子函数运行 | 在 owner 进程，所有键都触发 | 仅匹配的 hotkey 触发 WM_HOTKEY |
| 性能影响 | 每个键都过钩子（>300ms 超时机制） | 仅匹配键，零开销 |
| 协议风险 | 可被 RDP/UAC 提升过程绕过 | 无 |
| 卸载难度 | 进程异常时全局残留 | 窗口销毁自动释放 |

**结论**：`RegisterHotKey` 是 IME 拦截特定热键的"标准答案"。其它老牌 IME 都用这条路。

## WindInput 中的实现

### 动态注册策略

| 热键 | 注册时机 | 卸载时机 |
|------|----------|----------|
| `Ctrl+0..9` (Pin) | 候选可见 **且** 持有 thread focus | 候选消失 / 失焦 |
| `Ctrl+Shift+0..9` (Delete) | 候选可见 **且** 持有 thread focus | 候选消失 / 失焦 |

**为什么动态注册**：用户要求在"无输入状态"下让 Ctrl+1..9 透传给 QQ（切 Tab）。`RegisterHotKey` 一旦注册便无条件消费，无法运行时按状态决定。所以必须根据候选可见性动态注册/卸载。

**为什么必须叠加 thread focus 门控**：TSF IME DLL 在每个使用 TSF 的应用进程独立加载，每个进程都有一份 `CTextService`。`RegisterHotKey` 在 Windows 中**系统全局独占**——`(modifiers, vk)` 二元组同一时刻只能被一个进程持有，后到者得到 `err=1409`。如果不门控，多个后台进程的 IME 实例都试图注册，前台应用反而抢不到热键，按下时由"错的进程"处理。绑定 `ITfThreadFocusSink::OnSetThreadFocus / OnKillThreadFocus`：只有持有 thread focus 的实例才注册，失焦立即让出。

### 代码结构

文件：`wind_tsf/src/TextService.cpp`、`wind_tsf/include/TextService.h`、`wind_tsf/src/KeyEventSink.cpp`

1. **消息窗口**：`CTextService::_InitHotkeyWindow()` 在 `Activate` 时创建
   - `RegisterClassExW` 注册类，Debug/Release 后缀区分：`WindInputHotkeyWnd` / `WindInputHotkeyWndDebug`（与 pipe / CLSID 等其他跨进程资源的命名约定一致）
   - `CreateWindowExW(HWND_MESSAGE, ...)` 创建消息专用隐藏窗口
   - `SetWindowLongPtrW(GWLP_USERDATA, this)` 存 CTextService 指针供 WndProc 用
   - 启动 500ms `WM_TIMER`（`kFocusCheckTimerId`）用于 thread focus 自检兜底

2. **窗口过程**：`_HotkeyWndProc`
   - **race check（防焦点切换瞬间错位）**：进入 `WM_HOTKEY` 先用 `GetForegroundWindow` 比对自己的 PID，不是前台就立即 unregister 全部热键、向前台进程的 IME hidden window `PostMessageW` 一条 retry 消息，并丢弃本次按键
   - **WM_TIMER（500ms 自检）**：兜底 `OnKillThreadFocus` 在 Wails/Chromium 类宿主可能不触发，定时复核前台 PID，发现自己已经不是前台但还持有热键就主动让出 + 发 retry
   - **retry 消息（`RegisterWindowMessageW("WindInputHotkeyRetry_v1")`）**：让出热键的进程通知新前台 IME 立即重新尝试注册，避免它要等到下次候选可见才发现热键空了
   - **正常路径**：从 `wParam` 解码 hotkey id：
     - `kHotkeyIdPinBase + N` (0x4000..0x4009) → `('0'+N, KEYMOD_CTRL)`
     - `kHotkeyIdDelBase + N` (0x4010..0x4019) → `('0'+N, KEYMOD_CTRL|KEYMOD_SHIFT)`
   - 通过 `CKeyEventSink::DispatchHotkey(vk, mods)` 走与 `OnKeyDown` 相同的"send IPC + handle response"通路

3. **动态注册**：`NotifyCandidatesVisibilityChanged(BOOL hasCandidates)`
   - `hasCandidates && !_hotkeysActive` → `_RegisterCandidateHotkeys()` 注册 20 个（仅当 `_hasThreadFocus = TRUE`，闸门在函数头）
   - `!hasCandidates && _hotkeysActive` → `_UnregisterCandidateHotkeys()` 卸载

4. **Thread focus 钩子**：`ITfThreadFocusSink::OnSetThreadFocus / OnKillThreadFocus`
   - 拿到 thread focus → 置 `_hasThreadFocus = TRUE`（候选热键下次候选出现时自然补回）
   - 失去 thread focus → 置 `_hasThreadFocus = FALSE` + 立即 `_UnregisterCandidateHotkeys()`
   - `_InitHotkeyWindow` 末尾用 `GetForegroundWindow` + `GetWindowThreadProcessId` 自检种子，避免 IME 首次激活时 TSF 不主动派 `OnSetThreadFocus` 导致永远注册不上

5. **`MOD_NOREPEAT` 标志**：`RegisterHotKey(... MOD_NOREPEAT)` 避免按住键时连发 `WM_HOTKEY`，与 IME 候选选择/删除的"单次操作"语义吻合。

### 派发路径

```
用户按 Ctrl+2（候选可见状态）
   ↓
Windows 消息派发器匹配 hotkey id=0x4002
   ↓
WM_HOTKEY 投递到 _hHotkeyWnd 队列（在宿主 UI 线程）
   ↓
宿主消息泵 → DispatchMessage → _HotkeyWndProc
   ↓
解码：vk=0x32 ('2'), mods=KEYMOD_CTRL
   ↓
CKeyEventSink::DispatchHotkey(vk, mods)
   ↓
_SendKeyToService(vk, mods, KEY_EVENT_DOWN)
   ↓ IPC 到 Go 服务
Go 端 HandleKeyEvent → matchCandidateActionKey → handlePinCandidateByKey
   ↓ 返回 Consumed
C++ 端 _HandleServiceResponse
   ↓
返回，WM_KEYDOWN 从未产生，QQ 完全感知不到 Ctrl+2 被按下
```

## 故障排查

### `RegisterCandidateHotkeys: registered=N/20`，N < 20
- **err=1409 (ERROR_HOTKEY_ALREADY_REGISTERED)**：同一组合键已被其他进程注册。`RegisterHotKey` 是**系统全局独占**——通常说明另一个 IME 实例（或本 IME 的另一个变体如 Debug/Release）正持有热键
- 检查 thread focus 门控是否生效：日志里搜索 `OnSetThreadFocus` / `OnKillThreadFocus` / `FocusCheck timer` 序列
- 罕见情况：宿主自己注册了同样 hotkey（如 Edge 注册过 Ctrl+T）
- 处理：500ms 自检 timer 会兜底，新前台 IME 实例下一次候选可见时通过 retry 消息或自然 NotifyCandidatesVisibilityChanged 即可重新注册

### `WM_HOTKEY` 被错误的进程处理
- 焦点切换瞬间 race：旧前台进程的 `OnKillThreadFocus` 还没派到，WM_HOTKEY 已经投到它的 hidden window
- `_HotkeyWndProc` 入口的 race check (`GetForegroundWindow` 比对 own PID) 会捕获此场景，**当次按键丢弃**，同时向新前台进程 PostMessage retry
- 日志线索：`WM_HOTKEY race: not foreground (fgPid=... ownPid=...), releasing hotkeys` + `Posted retry to foreground IME hwnd=... pid=...`

### `WM_HOTKEY` 没有触发
- 检查日志 `Hotkey window created hwnd=0x...` 是否出现
- 检查 `_InitHotkeyWindow: initial thread focus seed=...` 是否为 1
- 检查 `RegisterCandidateHotkeys: registered=...` 是否在候选出现时被调用
- 可能宿主使用 raw input 而非标准消息（极少见）

### Debug / Release 版本互抢热键
- 两版本的 window class 后缀已经区分（`WindInputHotkeyWnd` vs `WindInputHotkeyWndDebug`），便于 Spy++ 诊断
- 但 `RegisterHotKey` 本身按 `(modifiers, vk)` 全局独占——thread focus 门控保证同一时刻只有前台进程注册，两版本天然接力
- 如果观察到一个版本"霸占"热键，看是否对应进程未触发 `OnKillThreadFocus`（Wails/Chromium 类宿主常见），500ms `FocusCheck timer` 应在半秒内兜底让出

### 性能
- 每次候选可见/消失各执行 20 次 RegisterHotKey / UnregisterHotKey
- 每次调用 < 1μs
- 500ms timer 自检每次仅一次 `GetForegroundWindow + GetWindowThreadProcessId`，开销可忽略

### 与 IME 切换的关系
- IME 被卸载时 `_UninitHotkeyWindow` → `DestroyWindow` → 系统自动释放所有 hotkey + KillTimer
- 同进程多次激活有兜底（`ERROR_CLASS_ALREADY_EXISTS=1410` 视为正常）

## 已知限制

1. **冲突场景**：如果宿主自身先注册了相同 hotkey（如某 Web 应用注册 `Ctrl+1`），我们的 `RegisterHotKey` 会失败，那个 hotkey 退化为 `WM_KEYDOWN` 路径
2. **多进程接力的瞬时盲区**：跨进程切焦点的几十毫秒内，`OnKillThreadFocus` 还没派到、WM_HOTKEY 已经投递。race check 会消费这次按键并通知新前台 retry，但**用户的这一次按键被丢弃**（下次按键正常）。500ms timer 兜底覆盖了 TSF focus 通知完全不触发的极端宿主
3. **AddWord (`Ctrl+=`) 不在本通路**：曾尝试过，因 DispatchHotkey 不在 TSF 事件回调栈内、Chromium 类宿主同步 EditSession 被拒、跨进程 1409 互抢等多重原因回退到 `OnKeyDown` + `IsKeyDownChineseOnlyHotkey` 通路。代价是 QQNT 中 Ctrl+= 会同时触发 QQ 自身的图片放大快捷键——这是 QQNT 实现偏差，IME 侧无法消除，仅这一家产品有该问题

## 历史

| 日期 | 状态 |
|------|------|
| 2026-05-19 | 静态反汇编第三方 TSF IME，定位 `RegisterHotKey` 机制 |
| 2026-05-19 | 实现动态注册（候选可见时启用，消失时卸载） |
| 2026-05-20 | 实测在 QQ 中 Ctrl+1/2 正常拦截 + 透传 |
| 2026-05-20 | 尝试把 `Ctrl+=` AddWord 也接入本通路（commit f9a4677） |
| 2026-05-20 | 观察到 QQ/WPS/浏览器加词位置错乱、ESC 退不出、长按无效、多进程 1409 互抢，Debug/Release 共存时 Release 残留霸占热键 |
| 2026-05-20 | 引入 thread-focus 门控 + WM_HOTKEY race check + 500ms timer 自检 + 跨进程 retry message + Debug/Release 窗口后缀 |
| 2026-05-20 | **AddWord 回退** 到 `OnKeyDown` + `IsKeyDownChineseOnlyHotkey` 通路（commit 70d3be7），Pin/Delete 保留 RegisterHotKey 并保留上述全部加固机制 |

## 参考

- [RegisterHotKey - Win32 API](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerhotkey)
- [WM_HOTKEY message](https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-hotkey)
