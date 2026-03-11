<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# internal

## Purpose
输入法核心逻辑的内部包集合，不对模块外部暴露。包含从 IPC 通信到引擎计算、UI 渲染的完整实现链路。

## Subdirectories
| Directory | Purpose |
|-----------|---------|
| `bridge/` | Named Pipe IPC 服务端，与 C++ TSF 桥接层通信 |
| `candidate/` | 候选词数据结构和排序逻辑 |
| `control/` | 控制管道服务端，供设置应用调用 |
| `coordinator/` | 核心协调器，处理按键事件、模式切换、生命周期 |
| `dict/` | 词库系统（Trie、分层词库、短语、Shadow、用户词典） |
| `engine/` | 引擎管理器及拼音/五笔引擎实现 |
| `hotkey/` | 热键配置编译器 |
| `ipc/` | 底层 IPC 协议和 Named Pipe 服务端基础设施 |
| `state/` | IME 状态管理器 |
| `transform/` | 文本转换（全角/半角、中英文标点） |
| `ui/` | Windows 原生 UI 渲染（候选窗口、工具栏、Tooltip） |

## For AI Agents

### Working In This Directory
- 这些包只能被 `cmd/` 和同级 `internal/` 包引用，不得被 `pkg/` 引用
- 核心数据流：`bridge` → `coordinator` → `engine` → `candidate` → `bridge`（响应）
- UI 更新：`coordinator` → `ui.Manager`（通过 channel 发送 UICommand）

### Testing Requirements
- 各包独立测试：`go test ./internal/...`
- `engine/pinyin/` 和 `dict/` 有较多单元测试，修改时务必运行

### Common Patterns
- Windows 平台专属代码用 `_windows.go` 后缀（如 `binformat/mmap_windows.go`）
- 接口定义与实现分离（如 `dict.Dict`、`engine.Engine`、`bridge.MessageHandler`）

## Dependencies
### Internal
- `pkg/` 下的公共包
- `internal/` 包之间有依赖关系（见各子目录）

### External
- `golang.org/x/sys/windows`
- `github.com/Microsoft/go-winio`

<!-- MANUAL: -->
