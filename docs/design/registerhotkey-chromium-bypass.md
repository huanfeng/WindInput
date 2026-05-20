# Win32 RegisterHotKey 绕开 Chromium 宿主加速键双处理

## 问题背景

部分基于 Chromium / Electron 的应用（典型代表 **QQNT / QQ**）实现 TSF IME 集成时，**不遵循"IME 吃了就停"的标准约定**。表现：

- IME 在 `OnTestKeyDown` / `OnKeyDown` 设置 `pfEaten = TRUE`
- 但宿主的加速键处理（如 `Ctrl+1..9` 切换 Tab/会话）**与 IME 处理并行触发**
- 结果：用户按 `Ctrl+2` 既让 IME 置顶第二个候选，**又**让 QQ 切到第二个会话

这与标准 Chromium 文档矛盾（Chromium 设计是 IME 接受后跳过加速键），是 QQNT 自身 Electron 实现的偏差，IME 侧无法通过 TSF 接口改正。

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

### 静态/动态注册策略

| 热键 | 注册时机 | 卸载时机 |
|------|----------|----------|
| `Ctrl+0..9` (Pin) | 候选可见 | 候选消失 |
| `Ctrl+Shift+0..9` (Delete) | 候选可见 | 候选消失 |
| `Ctrl+=` (AddWord) | 中文模式 | 英文模式 |

**为什么动态注册**：用户要求在"无输入状态"下让 Ctrl+1..9 透传给 QQ（切 Tab）。`RegisterHotKey` 一旦注册便无条件消费，无法运行时按状态决定。所以必须根据 IME 状态动态注册/卸载。

### 代码结构

文件：`wind_tsf/src/TextService.cpp`、`wind_tsf/include/TextService.h`

1. **消息窗口**：`CTextService::_InitHotkeyWindow()` 在 `Activate` 时创建
   - `RegisterClassExW` 注册类（`WindInputHotkeyWnd`）
   - `CreateWindowExW(HWND_MESSAGE, ...)` 创建消息专用隐藏窗口
   - `SetWindowLongPtrW(GWLP_USERDATA, this)` 存 CTextService 指针供 WndProc 用

2. **窗口过程**：`_HotkeyWndProc`
   - 处理 `WM_HOTKEY`，从 `wParam` 解码 hotkey id
   - id 映射到 (vk, mods)：
     - `kHotkeyIdPinBase + N` (0x4000..0x4009) → `('0'+N, KEYMOD_CTRL)`
     - `kHotkeyIdDelBase + N` (0x4010..0x4019) → `('0'+N, KEYMOD_CTRL|KEYMOD_SHIFT)`
   - 通过 `CKeyEventSink::DispatchHotkey(vk, mods)` 走与 `OnKeyDown` 相同的"send IPC + handle response"通路

3. **动态注册**：`NotifyCandidatesVisibilityChanged(BOOL hasCandidates)`
   - `hasCandidates && !_hotkeysActive` → `_RegisterCandidateHotkeys()` 注册 20 个
   - `!hasCandidates && _hotkeysActive` → `_UnregisterCandidateHotkeys()` 卸载

4. **`MOD_NOREPEAT` 标志**：`RegisterHotKey(... MOD_NOREPEAT)` 避免按住键时连发 `WM_HOTKEY`，与 IME 候选选择/删除的"单次操作"语义吻合。

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
- 某些 hotkey 已被同进程其它窗口注册
- 罕见情况：宿主自己注册了同样 hotkey（如 Edge 注册过 Ctrl+T）
- 处理：日志记录哪些失败，影响仅限失败的那几个

### `WM_HOTKEY` 没有触发
- 检查日志 `Hotkey window created hwnd=0x...` 是否出现
- 检查 `RegisterCandidateHotkeys: registered=...` 是否在候选出现时被调用
- 可能宿主使用 raw input 而非标准消息（极少见）

### 性能
- 每次候选可见/消失各执行 20 次 RegisterHotKey / UnregisterHotKey
- 每次调用 < 1μs
- 整体开销可忽略

### 与 IME 切换的关系
- IME 被卸载时 `_UninitHotkeyWindow` → `DestroyWindow` → 系统自动释放所有 hotkey
- 同进程多次激活有兜底（`ERROR_CLASS_ALREADY_EXISTS=1410` 视为正常）

## 已知限制

1. **冲突场景**：如果宿主自身先注册了相同 hotkey（如某 Web 应用注册 `Ctrl+1`），我们的 `RegisterHotKey` 会失败，那个 hotkey 退化为 `WM_KEYDOWN` 路径
2. **多 IME 同时活跃**：理论上 TSF 同一时刻只一个 IME 实例，但如果用户快速切换，可能短暂出现两个实例都尝试注册（先到者赢）
3. **不能解决其它热键**：当前仅覆盖 `Ctrl+0..9` / `Ctrl+Shift+0..9`，后续如需加入 `Ctrl+=` AddWord 等，需要扩展注册集合并设计相应的状态触发条件

## 历史

| 日期 | 状态 |
|------|------|
| 2026-05-19 | 静态反汇编第三方 TSF IME，定位 `RegisterHotKey` 机制 |
| 2026-05-19 | 实现动态注册（候选可见时启用，消失时卸载） |
| 2026-05-20 | 实测在 QQ 中 Ctrl+1/2 正常拦截 + 透传 |

## 参考

- [RegisterHotKey - Win32 API](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerhotkey)
- [WM_HOTKEY message](https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-hotkey)
