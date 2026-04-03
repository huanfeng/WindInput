# Win11 开始菜单候选框 z-order 解决方案

> 状态：方案 A 已实现并稳定
> 最后更新：2026-04-03

## 1. 问题描述

Win11 开始菜单（SearchHost.exe）中使用清风输入法时，候选框窗口被开始菜单遮挡，用户可以正常输入但看不到候选词。

## 2. 根因分析

### 2.1 DWM Window Band 机制

Windows DWM 使用未文档化的 **Band 层级**管理窗口 z-order。Band 是绝对层级——**不同 Band 之间的窗口永远不会交叉**，无论 `HWND_TOPMOST`、`SetWindowPos`、还是 owner 关系如何设置。

实际 z-order 从低到高（注意：数值顺序 ≠ 实际层级顺序）：

```
ZBID_DEFAULT(0) < ZBID_DESKTOP(1) < ZBID_IMMERSIVE_MOGO(6) < ... < ZBID_UIACCESS(2)
```

| Band 值 | 名称 | 典型窗口 |
|---------|------|---------|
| 0 | ZBID_DEFAULT | 默认 |
| 1 | ZBID_DESKTOP | 普通桌面窗口（含 TOPMOST） |
| 2 | ZBID_UIACCESS | UIAccess 辅助工具、IME（高于所有 immersive） |
| 3 | ZBID_IMMERSIVE_IHM | 触摸键盘、手写面板 |
| 6 | ZBID_IMMERSIVE_MOGO | **开始菜单、搜索面板** |
| 16 | ZBID_SYSTEM_TOOLS | 任务管理器、explorer shell |

### 2.2 当前状态

- **清风输入法候选窗口**（wind_input.exe 创建）：Band=1 (ZBID_DESKTOP)
- **开始菜单**（SearchHost.exe）：Band=6 (ZBID_IMMERSIVE_MOGO)
- Band=1 的窗口**永远**无法显示在 Band=6 之上

### 2.3 各输入法的处理方式

| 输入法 | 候选窗口进程 | Band | 方案 |
|--------|------------|------|------|
| 小狼毫/RIME | DLL 内创建 | 与宿主同 Band | DLL 内直接渲染 |
| 微软拼音 | explorer.exe | Band=16 | 系统特权进程 |
| 微信输入法 | wetype_renderer | Band=2 | UIAccess + 数字签名 |
| 清风输入法 | wind_input.exe | Band=1 | **需要解决** |

## 3. 已排除的方案

### 3.1 HWND_TOPMOST / SetWindowPos

`TOPMOST` 只影响同 Band 内的 z-order，不能跨 Band。

### 3.2 SetWindowBand

需要 IAM 线程（仅 explorer.exe 的桌面 shell 线程持有），任何其他进程/DLL 均无法获取 IAM key。调用链：

```
NtUserAcquireIAMKey(&key)  → 仅 SetShellWindowsEx 调用过的线程可用
NtUserEnableIAMAccess(key, TRUE)  → 在当前线程启用 IAM
SetWindowBand(hwnd, 0, band)  → 需要当前线程已启用 IAM
```

### 3.3 Owned Window 继承 Band（GWLP_HWNDPARENT 代理窗口）

**已验证无效**。DWM Band 系统完全独立于 Win32 的 owner/child 关系。设置 `GWLP_HWNDPARENT` 后，owned window 的 z-order 规则仅在同 Band 内生效。

### 3.4 普通 CreateWindowExW 在宿主进程内

**已验证无效**。即使 DLL 运行在 SearchHost.exe（Band=6）内，使用 `CreateWindowExW` 创建的窗口仍然是 Band=1。`CreateWindowExW` 不使用 Band 机制。

## 4. 可行方案

### 方案 A：DLL 代理渲染窗口（推荐）

**已验证可行。无需代码签名。**

#### 4.1 原理

TSF DLL（wind_tsf.dll）运行在宿主进程内。使用未文档化 API `CreateWindowInBand` 在 DLL 内创建指定 Band 的 layered window，Go 服务通过 IPC 发送渲染好的位图，DLL 调用 `UpdateLayeredWindow` 显示。

#### 4.2 验证结果（2026-03-29）

在 SearchHost.exe (PID:78420) 内测试 `CreateWindowInBand`：

```
Band=1 (DESKTOP)  → ERROR_INVALID_PARAMETER (87) — immersive 进程不允许创建低 Band
Band=2 (UIACCESS) → ERROR_ACCESS_DENIED (5) — 需要 UIAccess 权限
Band=6 (MOGO)     → 成功 ✅ actualBand=6，窗口可见于开始菜单之上
```

**结论**：immersive 进程内 `CreateWindowInBand` 只允许创建与宿主同 Band 的窗口，这正是我们需要的。

#### 4.3 API 签名

```c
// user32.dll 未导出，需 GetProcAddress 动态获取
typedef HWND (WINAPI* CreateWindowInBand_t)(
    DWORD dwExStyle,
    ATOM atom,              // RegisterClassExW 返回的 atom（不是类名字符串）
    LPCWSTR lpWindowName,
    DWORD dwStyle,
    int X, int Y, int nWidth, int nHeight,
    HWND hWndParent,
    HMENU hMenu,
    HINSTANCE hInstance,
    LPVOID lpParam,
    DWORD dwBand            // ZBID_* 值
);

typedef BOOL (WINAPI* GetWindowBand_t)(HWND hwnd, DWORD* pdwBand);
```

#### 4.4 实现架构

```
┌─────────────────────────────────────────────────────────┐
│ 宿主进程 (SearchHost.exe, Band=6)                        │
│                                                          │
│  wind_tsf.dll                                            │
│  ├─ CreateWindowInBand(Band=6) → 候选框 layered window   │
│  ├─ 接收 Go 服务发来的位图数据                              │
│  └─ UpdateLayeredWindow() 渲染                            │
│                                                          │
└──────────────────────┬──────────────────────────────────┘
                       │ IPC (命名管道 / 共享内存)
┌──────────────────────┴──────────────────────────────────┐
│ Go 服务 (wind_input.exe, Band=1)                         │
│                                                          │
│  ├─ 渲染候选框 → image.RGBA                               │
│  ├─ 通过 IPC 发送位图数据和位置信息                         │
│  └─ 普通进程中仍使用自己的候选窗口 (Band=1)                  │
│                                                          │
└─────────────────────────────────────────────────────────┘
```

#### 4.5 实现要点

**DLL 端（C++）**：

1. **窗口创建**
   - `GetProcAddress(user32, "CreateWindowInBand")` 动态获取 API
   - 窗口样式：`WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`
   - 需要先 `RegisterClassExW` 获取 ATOM（`CreateWindowInBand` 不接受类名字符串）
   - 仅在 Band > 1 的宿主进程中创建（通过 `GetWindowBand` 检查宿主窗口）

2. **位图接收与渲染**
   - 接收 Go 发来的 BGRA 位图数据 + 位置 + 尺寸
   - `CreateDIBSection` → 写入像素 → `UpdateLayeredWindow`
   - 需要在 DLL 的消息处理线程中执行

3. **位置同步**
   - DLL 已有光标位置信息（`SendCaretPositionUpdate`）
   - 可直接在 DLL 端定位窗口，无需等 Go 的位置指令

4. **生命周期管理**
   - `ActivateEx` 时创建（如果宿主 Band > 1）
   - `Deactivate` 时销毁
   - 需要处理宿主进程崩溃的清理

**Go 端**：

1. **位图传输**
   - 通过 push pipe 或新增专用管道发送渲染结果
   - 协议：`CMD_CANDIDATE_BITMAP` = header(x, y, width, height) + BGRA pixels

2. **双路渲染**
   - 如果 DLL 端报告有 Band 窗口 → 发位图给 DLL 渲染
   - 否则 → 使用自己的 layered window 渲染（当前逻辑）

#### 4.6 重难点

| 难点 | 说明 | 应对策略 |
|------|------|---------|
| **位图传输性能** | 候选框 300×400 BGRA ≈ 480KB/帧，60fps=28MB/s | 共享内存 + 脏矩形优化 |
| **共享内存** | `CreateFileMapping` 跨进程共享，避免管道拷贝 | Go 用 `windows.CreateFileMapping`，DLL 端 `OpenFileMapping` |
| **同步机制** | Go 写完 → 通知 DLL → DLL 读取渲染 | Named Event 或管道信号 |
| **鼠标交互** | DLL 窗口收到鼠标事件，需转发给 Go | 通过 IPC 发送鼠标坐标和事件类型 |
| **DPI 适配** | 不同进程可能有不同 DPI 感知模式 | 统一使用 Per-Monitor DPI V2 |
| **API 兼容性** | `CreateWindowInBand` 未文档化 | 运行时检测，不可用时降级到 Go 窗口 |
| **多进程复用** | 多个宿主进程可能同时激活 | 每个宿主独立创建/销毁，Go 端按 active client 路由 |

---

### 方案 B：UIAccess + 数字签名（备选）

#### 5.1 原理

为 wind_input.exe 添加 UIAccess manifest 并进行代码签名。Windows 会将其窗口提升到 ZBID_UIACCESS（Band=2），该层级**高于所有 immersive Band**（包括开始菜单的 Band=6）。

#### 5.2 前提条件（缺一不可）

1. **Manifest**：`<requestedExecutionLevel level="asInvoker" uiAccess="true"/>`
2. **数字签名**：受信任根 CA 颁发的代码签名证书（非自签名）
3. **安装路径**：必须位于受保护目录（`%ProgramFiles%` 或 `%SystemRoot%`）

#### 5.3 优缺点

**优点**：
- 微软官方支持的机制
- 实现最简单——无需修改渲染架构，所有窗口自动 Band=2
- 稳定性好，跨 Windows 版本兼容

**缺点**：
- 代码签名证书费用（约 $200–400/年）
- 安装路径限制（必须在 Program Files，不能随意放置）
- 开发调试不便（每次构建都需签名才能测试 UIAccess 效果）
- 免费的 Let's Encrypt 等证书不能用于代码签名

#### 5.4 实现步骤

1. 获取代码签名证书（如 DigiCert、Sectigo、GlobalSign）
2. 修改 wind_input.exe 的 manifest 添加 `uiAccess="true"`
3. 构建后签名：`signtool sign /fd sha256 /tr http://timestamp.digicert.com /td sha256 /a wind_input.exe`
4. 安装程序需将 exe 放到 `%ProgramFiles%\WindInput\`
5. 启动方式无需改变（UIAccess 程序由 Windows 自动提权 Band）

## 6. 实现中发现并解决的问题（2026-04-03）

方案 A 实现后在开始菜单和任务栏搜索中遇到了一系列稳定性问题，以下是排查过程和最终解决方案。

### 6.1 首字符 pendingFirstShow 延迟导致候选框不显示

**根因**：`pendingFirstShow` 机制在首字符时延迟 100ms 等待 `OnLayoutChange` 提供精确光标位置。但在开始菜单中 TSF `RequestEditSession` 始终失败（`TF_E_NOLOCK`），`OnLayoutChange` 永远不触发。

**修复**：HostRender 模式下跳过 `pendingFirstShow` 延迟，直接显示候选窗。HostRender 本就使用 fallback 近似位置，等待精确位置无意义。

**文件**：`wind_input/internal/coordinator/handle_key_action.go`

### 6.2 焦点抖动清除 hostRenderFunc 导致渲染回退到不可见的本地窗口

**根因**：`HandleFocusLost` 会清除 `hostRenderFunc`。开始菜单搜索结果更新等内部操作触发 TSF 焦点变化 → `hostRenderFunc` 在 `showUI()` 入队到 UI 线程处理之间被清空 → `doShowCandidates` 回退到本地窗口（不可见）。

**修复**：`HandleFocusLost` 不再清除 `hostRenderFunc`。HostRender 绑定进程级（按 PID），`showUI()` 每次渲染前通过 `updateHostRenderState()` 按 `activeProcessID` 自动重新评估。

**文件**：`wind_input/internal/coordinator/handle_lifecycle.go`

### 6.3 SearchHost 不支持 TSF composition，composition 终止清空输入状态

**根因**：开始菜单搜索框不支持 TSF composition。DLL 每次设置 composition 文本后搜索框立即终止 → `OnCompositionUnexpectedlyTerminated` → Go 端 `HandleCompositionTerminated` 清空输入状态 + 隐藏候选框。

**修复**：HostRender 模式下忽略 `HandleCompositionTerminated`。候选框通过 Band 窗口独立渲染，不依赖 TSF composition。

**文件**：`wind_input/internal/coordinator/handle_lifecycle.go`

### 6.4 host render 失败时静默 return 导致候选框完全消失

**根因**：`doShowCandidates` 中 `hostRender()` 调用失败（如共享内存已关闭）后仍执行 `return`，既不通过 host render 显示，也不回退到本地窗口。

**修复**：失败时清除过期的 `hostRenderFunc`，回退到本地窗口渲染。下次 `showUI()` 时 `updateHostRenderState()` 自动恢复。

**文件**：`wind_input/internal/ui/manager_candidate.go`

### 6.5 旧连接清理 goroutine 竞态销毁新连接的共享内存

**根因**：DLL 断开管道再重连（同 PID）时，旧连接的清理 goroutine 中 `CleanupClient(PID)` 可能在新连接的 `SetupHostRender(PID)` 之后执行，误删新创建的 SharedMemory。

**修复**：为 `HostRenderState` 添加 `SetupSeq` 单调递增计数器。`CleanupClient` 接受 `expectedSeq` 参数，仅清理匹配版本的状态，新版本不受旧清理影响。

**文件**：`wind_input/internal/bridge/host_render.go`、`wind_input/internal/bridge/server.go`

### 6.6 同进程不同 Band 窗口需要动态切换

**根因**：SearchHost.exe 的不同搜索入口使用不同 band（开始菜单搜索 band=6，任务栏搜索 band=13）。原实现按 PID 只创建一个 HostWindow，切换场景后候选框在错误层级。

**修复**：
- HostWindow 的 Band 显示窗口与共享内存解耦
- 在 TSF 线程的 `_EnsureHostRenderSetup` 中检测 band 变化，调用 `UpdateBand()` 只重建显示窗口
- 共享内存和渲染线程不受影响，切换毫秒级完成

**文件**：`wind_tsf/src/HostWindow.cpp`、`wind_tsf/include/HostWindow.h`、`wind_tsf/src/TextService.cpp`

### 6.7 Band=13 窗口不可见（owner 关系）

**根因**：band=13 (ZBID_IMMERSIVE_SEARCH) 窗口通过 `UpdateLayeredWindow` 渲染成功、`IsWindowVisible` 返回 TRUE，但用户看不到。其他输入法的候选窗口有搜索窗口作为 Parent/Owner。

**修复**：`CreateWindowInBand` 时将同进程的前台窗口设为 owner。对于 `WS_POPUP` 窗口，Windows 保证 owned 窗口在 owner 之上显示。

**文件**：`wind_tsf/src/HostWindow.cpp`

### 6.8 渲染线程跨线程 DestroyWindow 导致卡死

**根因**：早期实现在渲染线程中调用 `_UpdateBandIfNeeded()` → `DestroyWindow()` + `EnumWindows()`，跨线程窗口销毁可能导致 SendMessage 死锁。

**修复**：band 变化检测移到 TSF 线程（`_EnsureHostRenderSetup`），渲染线程只负责读取共享内存和调用 `UpdateLayeredWindow`。

**文件**：`wind_tsf/src/HostWindow.cpp`、`wind_tsf/src/TextService.cpp`

## 7. 已知遗留问题

### 7.1 服务端重启后首字符候选框不显示

**触发条件**：手动重启 Go 服务端或服务端异常退出后，切换到使用 Host 渲染的应用（如开始菜单），输入第一个字符。

**现象**：
- 输入第一个字符时，嵌入文本正常显示，但候选框不出现（被隐藏在开始菜单之下）
- 输入第二个字符时，第一个字符被吃掉，候选框正常显示

**根因分析**：

服务端重启后，C++ 侧通过 `_DoFullStateSync` 发送 `IMEActivated` 重建连接。但 `IMEActivated` 不会在 Go 侧触发进程白名单设置——白名单是通过 `FocusGained` 消息建立的。因此 `_EnsureHostRenderSetup` 中的 `SendHostRenderRequest` 被 Go 侧的 `IsProcessWhitelisted` 检查拒绝，Host 窗口未能建立，候选框回退到 Go 窗口渲染（Band=1，被开始菜单遮挡）。

时序如下：

```
1. 服务端重启，C++ 侧检测到 NeedsStateSync
2. _DoFullStateSync → 发送 IMEActivated（不含进程白名单信息）
3. _EnsureHostRenderSetup → SendHostRenderRequest → 被 IsProcessWhitelisted 拒绝
4. 首字符按键处理 → 候选框用 Go 窗口渲染 → 被开始菜单遮挡
5. 后续交互（showUI 中的 updateHostRenderState 自愈）→ Host 渲染恢复
```

**影响范围**：仅在手动重启服务端或服务端异常退出时触发，正常使用不受影响。

**可能的修复方向**：
- 在 `_DoFullStateSync` 中补发 `FocusGained`，使进程加入白名单
- 或让 `IMEActivated` 的处理也设置进程白名单

## 8. 推荐路线

**短期**：实现方案 A（DLL 代理渲染），无需额外成本，技术可行性已验证。

**长期**：如果获得代码签名证书，可切换到方案 B，简化架构。两个方案可以共存——有签名时用 UIAccess，无签名时降级到 DLL 代理渲染。

## 9. 参考资料

- [Window z-order in Windows 10 – ADeltaX](https://blog.adeltax.com/window-z-order-in-windows-10/)
- [How to call SetWindowBand – ADeltaX](https://blog.adeltax.com/how-to-call-setwindowband/)
- [CreateWindowInBand from injected DLL – ADeltaX Gist](https://gist.github.com/ADeltaX/a0b5366f91df26c5fa2aeadf439346c9)
- [Raymond Chen: Why does Task Manager disappear briefly...](https://devblogs.microsoft.com/oldnewthing/20230502-00/?p=108131)
- [arcanine300/CreateWindowInBand – GitHub](https://github.com/arcanine300/CreateWindowInBand)
