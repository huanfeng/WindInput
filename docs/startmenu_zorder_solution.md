# Win11 开始菜单候选框 z-order 解决方案

> 状态：方案已验证，待实现
> 最后更新：2026-03-29

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

## 6. 推荐路线

**短期**：实现方案 A（DLL 代理渲染），无需额外成本，技术可行性已验证。

**长期**：如果获得代码签名证书，可切换到方案 B，简化架构。两个方案可以共存——有签名时用 UIAccess，无签名时降级到 DLL 代理渲染。

## 7. 参考资料

- [Window z-order in Windows 10 – ADeltaX](https://blog.adeltax.com/window-z-order-in-windows-10/)
- [How to call SetWindowBand – ADeltaX](https://blog.adeltax.com/how-to-call-setwindowband/)
- [CreateWindowInBand from injected DLL – ADeltaX Gist](https://gist.github.com/ADeltaX/a0b5366f91df26c5fa2aeadf439346c9)
- [Raymond Chen: Why does Task Manager disappear briefly...](https://devblogs.microsoft.com/oldnewthing/20230502-00/?p=108131)
- [arcanine300/CreateWindowInBand – GitHub](https://github.com/arcanine300/CreateWindowInBand)
