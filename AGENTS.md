<!-- Generated: 2026-03-13 | Updated: 2026-04-01 -->

# WindInput - 清风输入法

## Purpose

Windows 中文输入法，支持拼音和五笔双模式。采用 C++ TSF 框架 + Go 输入引擎 + Vue 3 设置界面的多语言混合架构。核心采用 **Schema（输入方案）驱动架构**，通过 YAML 方案文件定义引擎类型、词库配置和学习策略。

## Architecture

```
┌──────────────┐     IPC (Named Pipe)     ┌──────────────────┐
│  wind_tsf    │ ◄───────────────────────► │   wind_input     │
│  C++ DLL     │     Binary Protocol      │   Go Service     │
│  TSF Bridge  │                          │   Input Engine   │
└──────────────┘                          └──────────────────┘
                                                   ▲
                                                   │ Control IPC
                                                   ▼
                                          ┌──────────────────┐
                                          │  wind_setting    │
                                          │  Wails GUI       │
                                          │  Vue 3 Frontend  │
                                          └──────────────────┘

Schema 驱动流程:
  data/schemas/*.schema.yaml → SchemaManager → EngineFactory → Engine + Dict
```

- **wind_tsf**: C++17 DLL，实现 Windows TSF (Text Services Framework) 接口，负责系统级输入法注册和键盘事件捕获；新增 HostWindow 机制解决 Win11 开始菜单候选框 z-order 问题
- **wind_input**: Go 服务进程，Schema 驱动的核心输入引擎（拼音连续评分 + 五笔码表），候选词管理，UI 渲染；通过 CGO 直接调用系统 dwrite.dll（已移除独立 wind_dwrite.dll）
- **wind_setting**: Wails v2 桌面应用，Go 后端 + Vue 3 前端，提供用户设置和方案管理界面

## Key Files

| File | Description |
|------|-------------|
| `build_all.ps1` | PowerShell 一键构建脚本（Go 服务 + C++ DLL + Wails 设置界面 + 词库下载），支持 debug/release/skip 参数 |
| `dev.ps1` | 开发调试启动脚本 |
| `dev.bat` | dev.ps1 的 bat 包装 |
| `CLAUDE.md` | AI Agent 工作指南 |

## Subdirectories

| Directory | Purpose |
|-----------|---------|
| `wind_tsf/` | C++ TSF 桥接层 DLL (see `wind_tsf/AGENTS.md`) |
| `wind_input/` | Go 输入引擎服务 (see `wind_input/AGENTS.md`) |
| `wind_setting/` | Wails 设置界面应用 (see `wind_setting/AGENTS.md`) |
| `data/` | Schema 方案定义、词库源数据、默认配置文件 (see `data/AGENTS.md`) |
| `docs/` | 项目文档：design/ 设计方案、requirements/ 需求规划、testing/ 测试指南、archive/ 历史文档 (see `docs/AGENTS.md`) |
| `dict/` | 运行时词库数据（unigram 等） |
| `installer/` | 安装/卸载脚本 (see `installer/AGENTS.md`) |
| `scripts/` | 构建辅助和工具脚本（版本管理、诊断工具）(see `scripts/AGENTS.md`) |
| `pic/` | 项目截图和图片资源 |

## For AI Agents

### Working In This Directory
- 构建命令: `.\build_all.ps1` (PowerShell，支持 `-WailsMode debug/release/skip` 参数)
- 构建产物输出到 `build/` 目录
- 不要主动进行 git commit（功能未测试前）和 git push
- 每次修改完 Go 代码需运行 `go fmt`
- 前端代码修改完需格式化
- 不需要提醒输入法卸载相关事项

### Build Steps
1. `[1/6]` Go 服务: `cd wind_input && go build -ldflags "-H windowsgui" -o ../build/wind_input.exe ./cmd/service`
2. `[2/6]` C++ DLL: `cd wind_tsf/build && cmake .. && cmake --build . --config Release`（仅输出 wind_tsf.dll；wind_dwrite.dll 已移除，Go 侧通过 CGO 直接调用系统 dwrite.dll）
3. `[3/6]` 设置界面: `cd wind_setting && wails build [-debug]`
4. `[4/6]` 下载 rime-ice 拼音词库到 `.cache/rime/`
5. `[5/6]` 复制词库、Schema 配置和默认配置（config.yaml）到 `build/`
6. `[6/6]` 验证构建产物

### Testing Requirements
- Go 测试: `cd wind_input && go test ./...`
- 前端: `cd wind_setting/frontend && pnpm test`（如有）

### IPC Protocol
- wind_tsf ↔ wind_input: Named Pipe (`\\.\pipe\wind_input`) 使用自定义二进制协议
- wind_tsf ← wind_input: Push Pipe (`\\.\pipe\wind_input_push`) 异步状态推送
- wind_setting → wind_input: Control IPC 进行配置管理和热重载通知

## Dependencies

### External
- Go 1.24+ with toolchain go1.24.2
- CMake 3.15+ / MSVC (C++17)
- Wails v2 CLI
- pnpm (前端包管理)
- Node.js (前端构建)
- PowerShell (构建脚本)

### Data Sources
- 拼音词库: [雾凇拼音 rime-ice](https://github.com/iDvel/rime-ice)
- 五笔词库: Rime 生态格式（自描述加载）

<!-- MANUAL: Any manually added notes below this line are preserved on regeneration -->
