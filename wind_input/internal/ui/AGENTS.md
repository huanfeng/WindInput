<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# internal/ui

## Purpose
Windows 原生 UI 渲染层。使用 Win32 API 实现输入法的所有可见界面元素：候选词窗口、工具栏、状态指示器（Tooltip）、弹出右键菜单。UI 运行在独立 goroutine 的 Windows 消息循环中，通过 channel 接收来自 coordinator 的命令。

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
| `renderer.go` | `Renderer`：GDI 渲染候选词列表（文字、颜色、高亮） |
| `renderer_layout.go` | 候选窗口布局计算（水平/垂直排列，DPI 感知） |
| `toolbar_window.go` | 工具栏 Win32 窗口创建和消息循环 |
| `toolbar_window_event.go` | 工具栏鼠标事件（拖拽、按钮点击） |
| `toolbar_renderer.go` | 工具栏 GDI 渲染（模式按钮、全角按钮、标点按钮、设置按钮） |
| `popup_menu.go` | `PopupMenu`：自定义弹出菜单窗口（替代系统菜单，支持主题） |
| `popup_menu_event.go` | 弹出菜单事件处理 |
| `popup_menu_render.go` | 弹出菜单 GDI 渲染 |
| `tooltip.go` | Tooltip（编码提示）窗口渲染 |
| `monitor.go` | 多显示器支持：获取目标显示器工作区，用于窗口位置计算 |
| `dpi.go` | DPI 缩放工具函数 |
| `protocol.go` | UI 内部消息类型（`UICommand`、`Candidate`、`ToolbarState`、`MenuItem`） |

## For AI Agents

### Working In This Directory
- UI 线程（Windows 消息循环）与 coordinator goroutine 通过 `chan UICommand` 通信
- `Manager.Start()` 创建窗口并进入消息循环（阻塞，必须在独立 goroutine 运行）
- `Manager.WaitReady()` 阻塞直到 UI 线程初始化完成（main.go 中等待）
- GDI 渲染：所有绘制在 `WM_PAINT` 中进行，使用双缓冲避免闪烁
- 候选窗口位置根据光标坐标（CaretX/Y）和显示器工作区自动调整，防止超出屏幕
- 工具栏支持拖拽移动，位置持久化到配置（`cfg.Toolbar.X`/`Y`）
- 主题颜色通过 `pkg/theme.Theme` 注入到渲染器
- `UnifiedMenuState` 用于构建统一的右键菜单（`BuildUnifiedMenuItems`）

### Testing Requirements
- UI 代码高度依赖 Windows GDI/Win32，无法做纯 Go 单元测试
- 视觉效果需在 Windows 环境下手动验证
- 布局计算逻辑（`renderer_layout.go`）可提取纯函数单独测试

### Common Patterns
- `UICommand.Type` 字符串值：`"show"`、`"hide"`、`"mode"`、`"toolbar_show"`、`"toolbar_hide"`、`"toolbar_update"`、`"settings"`、`"hide_menu"`、`"show_unified_menu"`
- 修改渲染逻辑后需检查水平/垂直两种候选布局
- DPI 变化（`WM_DPICHANGED`）触发字体和尺寸重新计算

## Dependencies
### Internal
- `pkg/theme` — 主题颜色定义

### External
- `golang.org/x/sys/windows` — Win32 API（窗口、GDI、消息）

<!-- MANUAL: -->
