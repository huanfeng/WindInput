# Debug 构建变体设计文档

> **状态**: ✅ 已完成实现
> **最后更新**: 2026-04-02

## 1. 功能目标

创建可与正式版共存的 Debug 构建变体，用于日常开发调试，避免卸载正式版。

### 核心需求
- Debug 版拥有独立的 CLSID、管道名、安装目录、进程名
- 与正式版可同时安装、同时运行、互不干扰
- UI 上有明显的调试标识（红色圆点角标）
- 通过 `./dev d1`~`d0` 菜单快速构建和安装

## 2. 差异化对照

| 项目 | 正式版 | 调试版 |
|------|--------|--------|
| DLL | `wind_tsf.dll` | `wind_tsf_debug.dll` |
| 主程序 | `wind_input.exe` | `wind_input_debug.exe` |
| 设置程序 | `wind_setting.exe` | `wind_setting_debug.exe` |
| CLSID 首段 | `99C2EE30` | `99C2DEB0` |
| Bridge 管道 | `\\.\pipe\wind_input` | `\\.\pipe\wind_input_debug` |
| Push 管道 | `\\.\pipe\wind_input_push` | `\\.\pipe\wind_input_debug_push` |
| Control 管道 | `\\.\pipe\wind_input_control` | `\\.\pipe\wind_input_debug_control` |
| 安装目录 | `%ProgramFiles%\WindInput` | `%ProgramFiles%\WindInputDebug` |
| 配置目录 | `%APPDATA%\WindInput` | `%APPDATA%\WindInputDebug` |
| 缓存目录 | `%LOCALAPPDATA%\WindInput` | `%LOCALAPPDATA%\WindInputDebug` |
| 构建输出 | `build\` | `build_debug\` |
| UI 标识 | 无 | 右上角红色圆点 |

## 3. 技术方案

### 编译时注入

- **Go 侧**: 通过 ldflags 注入 `buildvariant.variant=debug`，运行时读取 `buildvariant` 包的导出函数
- **C++ 侧**: 通过 CMake `-DWIND_DEBUG_VARIANT=ON` 注入预处理宏 `WIND_DEBUG_VARIANT`

### 架构

```
buildvariant 包（单一来源）
├── IsDebug()      → bool
├── Suffix()       → "" | "_debug"
├── AppName()      → "WindInput" | "WindInputDebug"
└── DisplayName()  → "清风输入法" | "清风输入法 (Debug)"

所有差异化标识均通过此包间接获取：
  config/paths.go    → 配置目录
  bridge/server.go   → IPC 管道名
  control/protocol.go → 控制管道名
  cmd/service/main.go → 互斥锁名
  cmd/service/logging.go → 日志路径
  ui/manager_config.go → 设置程序启动路径
```

C++ 侧通过 `#ifdef WIND_DEBUG_VARIANT` 切换：
- `Globals.cpp` — 5 个 GUID 定义
- `Globals.h` — 管道名、显示名
- `FileLogger.h/cpp` — 日志目录和文件名

### UI 角标

使用 SDF（Signed Distance Field）方式在 `*image.RGBA` 上绘制抗锯齿红色圆点，从右上角内缩 8px 避开圆角矩形边界。所有窗口（候选框、工具栏、提示、弹出菜单）在渲染末尾统一调用 `DrawDebugBanner(img)`。

选择像素级绘制而非 gg 库的原因：`gogpu/gg` 的 `NewContextForImage` 会对整张图片做两次全量像素复制，对 4px 红点来说开销不合理。

## 4. 开发菜单

```
./dev 0   → 正式版：构建设置 / 部署设置
./dev d1  → 调试版：卸载 / 构建(Release) / 安装
./dev d2  → 调试版：卸载 / 构建(Debug) / 安装
./dev d3  → 调试版：构建(Release)
./dev d4  → 调试版：构建(Debug)
./dev d5  → 调试版：安装
./dev d6  → 调试版：卸载
./dev d7  → 调试版：卸载 / 安装
./dev d0  → 调试版：构建设置 / 部署设置
```

## 5. 涉及文件

| 文件 | 改动 |
|------|------|
| `wind_input/pkg/buildvariant/variant.go` | **新建** — 构建变体标识中心 |
| `wind_input/pkg/config/paths.go` | AppName → buildvariant.AppName() |
| `wind_input/internal/bridge/server.go` | 管道名追加 Suffix() |
| `wind_input/pkg/control/protocol.go` | 控制管道名追加 Suffix() |
| `wind_input/cmd/service/main.go` | 互斥锁、弹框标题、启动日志 |
| `wind_input/cmd/service/logging.go` | 日志目录和文件名 |
| `wind_input/internal/ui/renderer.go` | DrawDebugBanner() 抗锯齿红点 |
| `wind_input/internal/ui/renderer_layout.go` | 候选框调用角标 |
| `wind_input/internal/ui/toolbar_renderer.go` | 工具栏调用角标 |
| `wind_input/internal/ui/tooltip.go` | 提示窗调用角标 |
| `wind_input/internal/ui/popup_menu_render.go` | 弹出菜单调用角标 |
| `wind_input/internal/ui/manager_config.go` | 设置程序启动路径区分 |
| `wind_tsf/CMakeLists.txt` | WIND_DEBUG_VARIANT 选项，输出名参数化 |
| `wind_tsf/wind_tsf.def.in` | **新建** — .def 模板 |
| `wind_tsf/include/Globals.h` | 管道名/显示名条件编译 |
| `wind_tsf/src/Globals.cpp` | 5 个 GUID 条件编译 |
| `wind_tsf/include/FileLogger.h` | 日志路径宏 |
| `wind_tsf/src/FileLogger.cpp` | 使用日志路径宏 |
| `wind_setting/app.go` | GetVersion() 加 Debug 标识，新增 IsDebugVariant() |
| `wind_setting/app_service.go` | 日志/配置目录使用 buildvariant |
| `wind_setting/app_tsf_log.go` | TSF 日志配置路径使用 buildvariant |
| `build_all.ps1` | -DebugVariant 参数，独立输出目录 |
| `installer/install.ps1` | 参数化所有文件名/目录/注册表 |
| `installer/uninstall.ps1` | 参数化卸载 |
| `dev.ps1` | 菜单重构，d1~d0 调试版完整操作 |
