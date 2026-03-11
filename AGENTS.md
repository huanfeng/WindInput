<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# WindInput - 清风输入法

## Purpose

Windows 中文输入法，支持拼音和五笔双模式。采用 C++ TSF 框架 + Go 输入引擎 + Vue 3 设置界面的多语言混合架构。

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
```

- **wind_tsf**: C++17 DLL，实现 Windows TSF (Text Services Framework) 接口，负责系统级输入法注册和键盘事件捕获
- **wind_input**: Go 服务进程，核心输入引擎（拼音 DAG/Viterbi + 五笔码表），候选词管理，UI 渲染
- **wind_setting**: Wails v2 桌面应用，Go 后端 + Vue 3 前端，提供用户设置界面

## Key Files

| File | Description |
|------|-------------|
| `build_all.bat` | 一键构建脚本（Go 服务 + C++ DLL + Wails 设置界面 + 词库下载） |
| `dev.bat` | 开发调试启动脚本 |
| `CLAUDE.md` | AI Agent 工作指南 |

## Subdirectories

| Directory | Purpose |
|-----------|---------|
| `wind_tsf/` | C++ TSF 桥接层 DLL (see `wind_tsf/AGENTS.md`) |
| `wind_input/` | Go 输入引擎服务 (see `wind_input/AGENTS.md`) |
| `wind_setting/` | Wails 设置界面应用 (see `wind_setting/AGENTS.md`) |
| `docs/` | 项目文档 (see `docs/AGENTS.md`) |
| `dict/` | 词库数据文件 (see `dict/AGENTS.md`) |
| `config_examples/` | 配置示例文件 (see `config_examples/AGENTS.md`) |
| `installer/` | 安装/卸载脚本 (see `installer/AGENTS.md`) |

## For AI Agents

### Working In This Directory
- 构建命令: `build_all.bat` (支持 debug/release/skip 参数)
- 构建产物输出到 `build/` 目录
- 不要主动进行 git commit（功能未测试前）和 git push
- 每次修改完 Go 代码需运行 `go fmt`
- 前端代码修改完需格式化
- 不需要提醒输入法卸载相关事项

### Build Steps
1. `[1/6]` Go 服务: `cd wind_input && go build -o ../build/wind_input.exe ./cmd/service`
2. `[2/6]` C++ DLL: `cd wind_tsf/build && cmake .. && cmake --build . --config Release`
3. `[3/6]` 设置界面: `cd wind_setting && wails build [-debug]`
4. `[4/6]` 下载 rime-ice 拼音词库到 `.cache/rime/`
5. `[5/6]` 复制词库到 `build/dict/`
6. `[6/6]` 验证构建产物

### Testing Requirements
- Go 测试: `cd wind_input && go test ./...`
- 前端: `cd wind_setting/frontend && pnpm test`（如有）

### IPC Protocol
- wind_tsf ↔ wind_input: Named Pipe (`\\.\pipe\WindInput`) 使用自定义二进制协议
- wind_setting → wind_input: Control IPC 进行配置管理

## Dependencies

### External
- Go 1.24+ with toolchain go1.24.2
- CMake 3.15+ / MSVC (C++17)
- Wails v2 CLI
- pnpm (前端包管理)
- Node.js (前端构建)

### Data Sources
- 拼音词库: [雾凇拼音 rime-ice](https://github.com/iDvel/rime-ice)
- 五笔词库: 极爽词库6

<!-- MANUAL: Any manually added notes below this line are preserved on regeneration -->
