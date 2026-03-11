<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# wind_setting

## Purpose
清风输入法（WindInput）的图形化设置界面。基于 Wails v2 构建，Go 后端负责读写配置文件和词库，Vue 3 前端提供设置 UI。编译后嵌入为单一可执行文件（`wind_setting.exe`），由主程序托盘菜单启动，支持通过命令行参数 `--page=<name>` 直接跳转到指定页面。

## Key Files
| 文件 | 说明 |
|------|------|
| `main.go` | 程序入口：解析 `--page` 参数，初始化 Wails App，注册 Go 绑定 |
| `app.go` | `App` 结构体定义及生命周期（startup/shutdown），初始化各编辑器和文件监控 |
| `app_config.go` | 配置读写 API：`GetConfig`、`SaveConfig`、`ReloadConfig`、`CheckConfigModified` |
| `app_dict.go` | 词库管理 API：短语（Phrase）、用户词库（UserDict）、Shadow 规则，含导入/导出 |
| `app_service.go` | 服务控制 API：`CheckServiceRunning`、`NotifyReload`、主题管理、文件变化检测 |
| `wails.json` | Wails 项目配置，前端包管理器为 pnpm |
| `go.mod` | Go 模块：`wind_setting`，依赖 `wind_input`（本地 replace）和 `wailsapp/wails/v2 v2.11.0` |

## Subdirectories
| 目录 | 说明 |
|------|------|
| `internal/` | Go 内部包：editor（编辑器）和 filesync（文件监控） |
| `frontend/` | Vue 3 + TypeScript 前端 |
| `build/` | Wails 构建资源（图标、Windows manifest、安装包脚本） |

## For AI Agents
### Working In This Directory
- Go 后端方法自动绑定为 Wails JS API，前端通过 `wailsjs/go/main/App` 调用
- 所有绑定方法定义在 `app*.go` 中，方法名即为前端调用名（PascalCase）
- 支持双模式运行：Wails 环境（生产）通过 IPC 调用 Go；HTTP 模式（开发调试）通过 REST API
- 命令行参数格式：`wind_setting.exe --page=dictionary` 或 `--dictionary`
- 有效页面名：`general`、`input`、`hotkey`、`appearance`、`dictionary`、`advanced`、`about`
- 保存配置后自动调用 `NotifyReload` 通知主程序热重载（goroutine 异步）

### Testing Requirements
- Go 构建：`wails build` 或 `go build ./...`（在 wind_setting 目录下）
- 前端构建：`pnpm run build`（在 frontend 目录下）
- 开发模式：`wails dev`（同时启动 Go 和 Vite 开发服务器）
- 格式化：Go 修改后运行 `go fmt ./...`；前端修改后运行格式化
- 功能测试须在完整 Wails 环境中进行，确保 IPC 绑定正常

### Common Patterns
- 每次写入文件后调用 `a.fileWatcher.UpdateState(path)` 更新快照，防止误报外部修改
- 配置保存后异步 `go a.NotifyReload(target)` 通知主程序
- 前端通过 `isWailsEnv` 判断运行环境，自动切换 API 来源

## Dependencies
### Internal
- `wind_setting/internal/editor` — 配置/词库文件编辑器
- `wind_setting/internal/filesync` — 文件变化监控
- `github.com/huanfeng/wind_input/pkg/config` — 配置加载/保存
- `github.com/huanfeng/wind_input/pkg/dictfile` — 词库文件格式
- `github.com/huanfeng/wind_input/pkg/control` — 控制管道客户端
- `github.com/huanfeng/wind_input/pkg/theme` — 主题管理

### External
- `github.com/wailsapp/wails/v2 v2.11.0` — 桌面应用框架
- Vue 3、TypeScript、Vite（前端）

<!-- MANUAL: -->
