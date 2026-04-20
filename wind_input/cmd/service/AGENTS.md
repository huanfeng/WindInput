<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-20 -->

# cmd/service

## Purpose
清风输入法主服务进程入口。负责初始化所有组件、编排生命周期，并运行 Bridge Named Pipe 主循环（阻塞 main goroutine）。

启动流程：设置 DPI 感知 → 加载配置 → 单例检查 → 初始化日志 → 初始化常用字表 → 加载 Schema → 加载词库 → 初始化引擎 → 启动 UI → 创建 Coordinator → 启动 Control Pipe → 创建 HostRenderManager → 启动 Bridge → 监听退出/重启信号。

## Key Files
| File | Description |
|------|-------------|
| `main.go` | 服务入口，组件初始化、生命周期管理、热重载实现（约 380 行） |
| `logging.go` | 日志轮转（`rotatingWriter`）和多路 slog Handler（`multiHandler`），5MB × 3 轮转 |
| `version.go` | 版本号变量，通过 ldflags 在构建时注入（`-X main.version=x.y.z`） |
| `winres/winres.json` | Windows 资源文件配置（版本信息、图标、清单），由 go-winres 工具生成 |
| `rsrc_windows_amd64.syso` | 64 位 Windows 资源对象文件（由 winres.json 生成，提交到仓库） |
| `rsrc_windows_386.syso` | 32 位 Windows 资源对象文件（由 winres.json 生成） |

## For AI Agents

### Working In This Directory
- **启动流程**（main 函数执行顺序）：
  1. DPI 感知设置（`setDPIAwareness()`）
  2. 加载配置（`config.LoadConfig()`）
  3. 单例检查（`checkSingleton()` + Named Mutex）
  4. 日志初始化（`initLogging()` + 轮转 Writer）
  5. 常用字表初始化
  6. Schema 加载（`schemaManager.Load()`）
  7. 词库加载（`dictManager.Load()`）
  8. 引擎初始化（`engine.New()`）
  9. UI 管理器启动（`uiManager.Start()` + `WaitReady()`）
  10. Coordinator 创建并启动
  11. Control Pipe 启动（控制管道服务）
  12. HostRenderManager 创建（Band 窗口代理渲染）
  13. Bridge Named Pipe 启动（阻塞 main goroutine）

- 单例保护：Windows Named Mutex（`Global\WindInput{Suffix}IMEService`）+ Pipe 存在性双重检查
- 日志文件路径：`%LOCALAPPDATA%\WindInput\logs\wind_input.log`（5MB × 3 轮转）
- 内存限制：`SetMemoryLimit(150MB)`，`SetGCPercent(50)`
- 数据根目录：`exeDir/data`（`config.GetDataDir(exeDir)`），Schema 和词库从此目录加载
- `--restart` 启动参数：重启时等待前一实例退出（`waitForPreviousExit()`）
- UI 必须在 Coordinator 之前就绪（`uiManager.WaitReady()`），确保 Coordinator 创建时 UI 已准备好

### Testing Requirements
- 需要在 Windows 环境下集成测试（依赖 Named Pipe 和 Windows API）
- 单元测试主要覆盖 `logging.go` 的轮转逻辑

### Common Patterns
- 组件通过接口传递（`BridgeServer`、`ReloadHandler`、`DictManager` 等），便于测试替换
- 退出/重启通过 channel 信号（`coordinator.ExitRequested()`、`coordinator.RestartRequested()`）
- 配置热重载流程（`reloadHandlerImpl.OnReload()`）：
  1. 更新 Schema（`schemaManager.SwitchSchema()` 如有必要）
  2. 重新加载引擎（`engine.Reload()`）
  3. 更新各模块配置（`UpdateHotkeyConfig()`、`UpdateUIConfig()`、`UpdateToolbarConfig()` 等）
- HostRenderManager 处理 Band 窗口代理渲染，解决 Win11 开始菜单 z-order 问题，进程白名单来自 `cfg.Advanced.HostRenderProcesses`
- DPI 感知防止 UI 模糊，使用 Windows 10+ API 优先（`SetProcessDpiAwareness`），回退到 Vista+ API（`SetProcessDPIAware`）

## Dependencies
### Internal
- `internal/bridge` — Named Pipe 服务端、HostRenderManager
- `internal/control` — 控制管道服务端
- `internal/coordinator` — 核心协调器
- `internal/dict` — 词库管理器
- `internal/engine` + `engine/pinyin` + `engine/wubi` — 引擎
- `internal/schema` — Schema 管理器
- `internal/ui` — UI 管理器
- `pkg/config` — 配置加载（三层合并）
- `pkg/control` — 控制管道协议

### External
- `golang.org/x/sys/windows` — Mutex、DPI API

<!-- MANUAL: -->
