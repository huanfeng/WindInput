<!-- Generated: 2026-03-13 | Updated: 2026-04-08 -->

# wind_input

## Purpose
清风输入法（WindInput）的 Go 服务模块，提供中文输入法的核心后端逻辑。作为独立进程运行，通过 Windows Named Pipe 与 C++ TSF（文本服务框架）桥接层通信。

采用 **Schema（输入方案）驱动架构**：通过 `*.schema.yaml` 定义引擎类型（拼音/码表）、词库配置和学习策略，由 SchemaManager 统一管理方案的加载、切换和运行时状态。

Go 模块：`github.com/huanfeng/wind_input`，Go 1.24，仅支持 Windows 平台。

近期主要新增功能：快捷加词（AddWord）对话框、Host 渲染管理器（解决 Win11 开始菜单 z-order 问题）、CGO DirectWrite 文本渲染、三层配置加载机制（代码默认值→系统预置→用户配置）、快捷键扩展（DeleteCandidate、PinCandidate、ToggleToolbar、OpenSettings、AddWord）。

## Key Files
| File | Description |
|------|-------------|
| `README.md` | 项目说明 |
| `go.mod` | Go 模块定义，依赖 go-winio、x/sys/windows、yaml.v3 |

## Subdirectories
| Directory | Purpose |
|-----------|---------|
| `cmd/` | 可执行程序入口点（service、词库生成工具） (see `cmd/AGENTS.md`) |
| `internal/` | 内部包（不对外暴露）(see `internal/AGENTS.md`) |
| `pkg/` | 公共包（供外部或多处引用）(see `pkg/AGENTS.md`) |
| `themes/` | 主题 YAML 数据文件 (see `themes/AGENTS.md`) |

## For AI Agents

### Working In This Directory
- 所有代码修改后需执行 `go build ./...` 确认编译通过
- 修改 Go 代码后需运行 `go fmt ./...` 格式化
- 主服务入口为 `cmd/service/main.go`
- 架构分层：`cmd` → `internal/coordinator` → `internal/schema` → `internal/engine` + `internal/dict` + `internal/ui` + `internal/bridge`

### Testing Requirements
- 运行单元测试：`go test ./...`
- 各 package 的测试文件与源码同目录（`*_test.go`）
- 功能未测试前不得提交

### Common Patterns
- Windows Named Pipe 用于进程间通信（bridge、control）
- `internal/` 包不对外暴露；公共类型放 `pkg/`
- 错误通过 `log/slog` 结构化日志记录
- 内存限制：150MB，GOGC=50（见 `cmd/service/main.go`）
- Schema YAML 文件驱动引擎创建和词库加载
- 三层配置加载：代码默认值 → `data/config.yaml`（系统预置）→ `%APPDATA%/WindInput/config.yaml`（用户配置）

## Dependencies
### Internal
- 所有 internal/ 和 pkg/ 子包

### External
- `golang.org/x/sys/windows` — Windows API 调用
- `github.com/Microsoft/go-winio` — Windows Named Pipe 高级封装
- `gopkg.in/yaml.v3` — YAML 配置文件解析

<!-- MANUAL: -->
