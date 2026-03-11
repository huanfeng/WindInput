<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# cmd/service

## Purpose
清风输入法主服务进程入口。负责初始化所有组件、编排生命周期，并运行 Bridge Named Pipe 主循环（阻塞 main goroutine）。

启动流程：设置 DPI 感知 → 加载配置 → 单例检查 → 初始化日志 → 加载词库 → 初始化引擎 → 启动 UI → 创建 Coordinator → 启动 Control Pipe → 启动 Bridge → 监听退出/重启信号。

## Key Files
| File | Description |
|------|-------------|
| `main.go` | 服务入口，组件初始化、生命周期管理 |
| `logging.go` | 日志轮转（`rotatingWriter`）和多路 slog Handler（`multiHandler`） |

## For AI Agents

### Working In This Directory
- 修改启动顺序时注意组件依赖关系：UI 必须在 Coordinator 之前就绪（`uiManager.WaitReady()`）
- 单例通过 Windows Named Mutex（`Global\WindInputIMEService`）和 Pipe 存在性双重检查
- 日志文件路径：`%LOCALAPPDATA%\WindInput\logs\wind_input.log`（5MB × 3 轮转）
- 内存策略：`SetMemoryLimit(150MB)`，`SetGCPercent(50)`
- `reloadHandlerImpl` 实现热重载，支持引擎类型切换、热键、UI、工具栏、输入配置

### Testing Requirements
- 需要在 Windows 环境下集成测试（依赖 Named Pipe 和 Windows API）
- 单元测试主要覆盖 `logging.go` 的轮转逻辑

### Common Patterns
- 组件通过接口传递（`BridgeServer`、`ReloadHandler`），便于测试替换
- 退出/重启通过 channel 信号（`coordinator.ExitRequested()`、`coordinator.RestartRequested()`）
- 五笔配置从 `DefaultConfig()` 基础上覆盖，避免遗漏新增字段

## Dependencies
### Internal
- `internal/bridge` — Named Pipe 服务端
- `internal/control` — 控制管道服务端
- `internal/coordinator` — 核心协调器
- `internal/dict` — 词库管理器
- `internal/engine` + `engine/pinyin` + `engine/wubi` — 引擎
- `internal/ui` — UI 管理器
- `pkg/config` — 配置加载

### External
- `golang.org/x/sys/windows` — Mutex、DPI API

<!-- MANUAL: -->
