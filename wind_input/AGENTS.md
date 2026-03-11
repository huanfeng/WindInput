<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# wind_input

## Purpose
清风输入法（WindInput）的 Go 服务模块，提供中文输入法的核心后端逻辑。作为独立进程运行，通过 Windows Named Pipe 与 C++ TSF（文本服务框架）桥接层通信，实现拼音和五笔两种输入引擎、候选词管理、UI 渲染和词库管理。

Go 模块：`github.com/huanfeng/wind_input`，Go 1.24，仅支持 Windows 平台。

## Key Files
| File | Description |
|------|-------------|
| `README.md` | 项目说明 |

## Subdirectories
| Directory | Purpose |
|-----------|---------|
| `cmd/` | 可执行程序入口点（service、工具命令） |
| `configs/` | 示例配置文件 |
| `internal/` | 内部包（不对外暴露） |
| `pkg/` | 公共包（供外部或多处引用） |
| `themes/` | 主题数据文件 |

## For AI Agents

### Working In This Directory
- 所有代码修改后需执行 `go build ./...` 确认编译通过
- 修改 Go 代码后需运行 `go fmt ./...` 格式化
- 主服务入口为 `cmd/service/main.go`
- 架构分层：`cmd` → `internal/coordinator` → `internal/engine` + `internal/ui` + `internal/bridge`

### Testing Requirements
- 运行单元测试：`go test ./...`
- 各 package 的测试文件与源码同目录（`*_test.go`）
- 功能未测试前不得提交

### Common Patterns
- Windows Named Pipe 用于进程间通信（bridge、control）
- `internal/` 包不对外暴露；公共类型放 `pkg/`
- 错误通过 `log/slog` 结构化日志记录
- 内存限制：150MB，GOGC=50（见 `cmd/service/main.go`）

## Dependencies
### Internal
- 所有 internal/ 和 pkg/ 子包

### External
- `golang.org/x/sys/windows` — Windows API 调用
- `github.com/Microsoft/go-winio` — Windows Named Pipe 高级封装
- `gopkg.in/yaml.v3` — YAML 配置文件解析

<!-- MANUAL: -->
