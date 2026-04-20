<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-20 -->

# internal/ui

## Purpose
Windows 原生 UI 渲染层。使用 Win32 API 实现输入法的所有可见界面元素：候选词窗口、工具栏、状态指示器（Tooltip）、弹出右键菜单。UI 运行在独立 goroutine 的 Windows 消息循环中，通过 channel 接收来自 coordinator 的命令。DirectWrite 渲染由纯 Go CGO 桥接实现，不再依赖外部 DLL。

## Key Files
| File | Description |
|------|-------------|
| `manager.go` | `Manager`：UI 管理器主体，channel 消息循环，`Start()`/`WaitReady()`/`UpdateCandidates()` 等 |
| `manager_candidate.go` | 候选窗口管理：显示/隐藏/更新候选列表和分页 |
| `manager_config.go` | 配置更新：字体、主题、布局、Tooltip 延迟等 |
| `manager_indicator.go` | 状态指示器（模式切换时短暂显示的浮动提示） |
| `manager_toolbar.go` | 工具栏管理：显示/隐藏/更新状态（中英文、全角、标点） |
| `window.go` | Win32 候选词窗口创建、WndProc、GDI 渲染 |
| `window_mouse.go` | 候选词窗口鼠标事件处理（点击选词、鼠标悬停） |
| `window_registry.go` | `WindowRegistry[T]`：泛型 HWND→`*T` 映射，供 WndProc 回调安全查找窗口实例 |
| `layered_window.go` | `UpdateLayeredWindowFromImage`：将 `image.RGBA` 渲染到分层窗口（`WS_EX_LAYERED`），处理 RGBA→BGRA 转换、CreateDIBSection、UpdateLayeredWindow |
| `text_backend.go` | `TextBackendManager`：统一管理 GDI/FreeType/DirectWrite 三种文字渲染后端的生命周期；`NewTextBackendManager(label)` 创建实例 |
| `dwrite_cgo_windows.go` | CGO 桥接文件（仅 Windows）：C trampoline `cDrawGlyphRunTrampoline` 从 XMM 寄存器正确接收 float 参数后转发给 Go 导出函数 `goDrawGlyphRunBridge`；解决 Windows x64 COM 回调中 float 参数无法通过 `syscall.NewCallback` 可靠提取的问题；`dwCGODrawGlyphRunCallback()` 返回 C 函数指针供 COM vtable 使用 |
| `dwrite_text.go` | DirectWrite 文字渲染实现（IDWriteFactory/IDWriteTextLayout COM 接口调用） |
| `renderer.go` | `Renderer`：GDI 渲染候选词列表（文字、颜色、高亮） |
| `renderer_layout.go` | 候选窗口布局计算（水平/垂直排列，DPI 感知） |
| `toolbar_window.go` | 工具栏 Win32 窗口创建和消息循环 |
| `toolbar_window_event.go` | 工具栏鼠标事件（拖拽、按钮点击） |
| `toolbar_renderer.go` | 工具栏 GDI 渲染（模式按钮、全角按钮、标点按钮、设置按钮） |
| `popup_menu.go` | `PopupMenu`：自定义弹出菜单窗口，支持子菜单、勾选状态、主题；`Show`/`Hide`/`Destroy`；键盘导航通过全局低级键盘钩子（`WH_KEYBOARD_LL`）实现；子菜单共享父菜单渲染资源（`newPopupMenuShared`） |
| `popup_menu_event.go` | 弹出菜单事件处理：鼠标移动/点击/离开事件、键盘导航（`handleKeyDown`：↑↓/←→/Enter/Esc/字母快捷键）、`checkMouseState` 跨进程点击检测；`menuKbNavActive` 抑制键盘导航时的幻象鼠标事件 |
| `popup_menu_render.go` | 弹出菜单 GDI 渲染 |
| `global_hotkey.go` | `GlobalHotkeyEntry`：全局热键定义；`ParseHotkeyString` 将配置字符串（如 `"ctrl+\`"`）解析为 `GlobalHotkeyEntry`；`globalHotkeyState`：通过 `RegisterHotKey`/`UnregisterHotKey` 管理线程级全局热键注册，`handleWMHotkey` 响应 `WM_HOTKEY` 消息并异步调用 callback；`resolveVK` 通用虚拟键码解析 |
| `tooltip.go` | Tooltip（编码提示）窗口渲染 |
| `monitor.go` | 多显示器支持：获取目标显示器工作区，用于窗口位置计算 |
| `dpi.go` | DPI 缩放工具函数 |
| `gdi_text.go` | GDI 文字渲染实现 |
| `font_config.go` | `FontConfig`：字体路径/大小/样式配置 |
| `text_drawer.go` | `TextDrawer` 接口：统一 GDI/DirectWrite 绘制 API |
| `protocol.go` | UI 内部消息类型（`UICommand`、`Candidate`、`ToolbarState`、`MenuItem`） |

## For AI Agents

### Working In This Directory
- UI 线程（Windows 消息循环）与 coordinator goroutine 通过 `chan UICommand` 通信
- `Manager.Start()` 创建窗口并进入消息循环（阻塞，必须在独立 goroutine 运行）
- `Manager.WaitReady()` 阻塞直到 UI 线程初始化完成（main.go 中等待）
- GDI 渲染：所有绘制在 `WM_PAINT` 中进行，使用双缓冲避免闪烁
- **分层窗口**（`layered_window.go`）：使用 `WS_EX_LAYERED` + `UpdateLayeredWindow` 实现透明背景，图像数据为预乘 alpha 的 BGRA 格式
- **窗口注册表**（`window_registry.go`）：泛型 `WindowRegistry[T]`，WndProc 中用 HWND 查找对应 Go 结构体，线程安全
- **文字后端**（`text_backend.go`）：`TextBackendManager` 嵌入到需要文字渲染的窗口结构体，统一管理后端切换
- **DirectWrite CGO 桥**（`dwrite_cgo_windows.go`）：仅在使用 CGO 构建时编译（`go build` 标签）；修改 vtable 回调时需同步修改 C trampoline 签名
- **弹出菜单键盘导航**：`installMenuKeyboardHook` 安装全局低级键盘钩子（必须在 UI 线程调用）；`menuKbNavActive`+`menuKbNavMouseX/Y` 用于抑制键盘操作后 Windows 产生的幻象鼠标事件
- 候选窗口位置根据光标坐标（CaretX/Y）和显示器工作区自动调整，防止超出屏幕
- 工具栏支持拖拽移动，位置持久化到配置（`cfg.Toolbar.X`/`Y`）
- 主题颜色通过 `pkg/theme.Theme` 注入到渲染器
- `UnifiedMenuState` 用于构建统一的右键菜单（`BuildUnifiedMenuItems`）

### Testing Requirements
- UI 代码高度依赖 Windows GDI/Win32，无法做纯 Go 单元测试
- `menu_disable_test.go` 是现有测试（菜单禁用状态逻辑）
- 视觉效果需在 Windows 环境下手动验证
- 布局计算逻辑（`renderer_layout.go`）可提取纯函数单独测试

### Common Patterns
- `UICommand.Type` 字符串值：`"show"`、`"hide"`、`"mode"`、`"toolbar_show"`、`"toolbar_hide"`、`"toolbar_update"`、`"settings"`、`"hide_menu"`、`"show_unified_menu"`
- 修改渲染逻辑后需检查水平/垂直两种候选布局
- DPI 变化（`WM_DPICHANGED`）触发字体和尺寸重新计算
- 新建窗口类型时使用 `WindowRegistry[T]` 管理 HWND 到 Go 实例的映射，避免全局变量
- 弹出菜单字母快捷键格式：菜单项文本末尾 `(X)` 括号内单字母，如 `"设置(S)"`

## Dependencies
### Internal
- `pkg/theme` — 主题颜色定义

### External
- `golang.org/x/sys/windows` — Win32 API（窗口、GDI、消息、分层窗口、RegisterHotKey）

<!-- MANUAL: -->
