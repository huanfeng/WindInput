<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-08 | Updated: 2026-04-20 -->

# internal

## Purpose
输入法核心逻辑的内部包集合，不对模块外部暴露。包含从 Schema 方案管理、IPC 通信到引擎计算、UI 渲染的完整实现链路。

## Subdirectories
| Directory | Purpose |
|-----------|---------|
| `bridge/` | Named Pipe IPC 服务端，与 C++ TSF 桥接层通信；宿主进程代理渲染（共享内存位图传输） (see `bridge/AGENTS.md`) |
| `candidate/` | 候选词数据结构和过滤/排序逻辑 (see `candidate/AGENTS.md`) |
| `clipboard/` | Windows 剪贴板读写操作 (see `clipboard/AGENTS.md`) |
| `coordinator/` | 核心协调器，处理按键事件、模式切换、生命周期（18 个处理器文件，支持加词和候选词操作） (see `coordinator/AGENTS.md`) |
| `dict/` | 词库系统（Trie、CompositeDict、短语、Shadow pin+delete、用户词典、词频） (see `dict/AGENTS.md`) |
| `engine/` | Schema 驱动的引擎管理器及拼音/五笔/混输引擎实现 |
| `hotkey/` | 热键配置编译器（支持候选词操作热键和通用按键绑定） |
| `ipc/` | 底层 IPC 协议和 Named Pipe 服务端基础设施 |
| `rpc/` | JSON-RPC IPC 服务端（词库、Shadow、短语、系统管理）(see `rpc/AGENTS.md`) |
| `schema/` | **输入方案管理**：Schema 定义、工厂、加载器、学习策略（方案驱动架构核心） |
| `store/` | 基于 bbolt 的持久化存储层（用户词、临时词、Shadow、词频） (see `store/AGENTS.md`) |
| `transform/` | 文本转换（全角/半角、中英文标点） |
| `ui/` | Windows 原生 UI 渲染（候选窗口、工具栏、Tooltip、DPI 感知）；DirectWrite 由 CGO 桥接实现，弹出菜单支持键盘导航和子菜单 |

## For AI Agents

### Working In This Directory
- 这些包只能被 `cmd/` 和同级 `internal/` 包引用，不得被 `pkg/` 引用
- 核心数据流：`bridge` → `coordinator` → `schema` → `engine` → `dict` → `candidate` → `bridge`（响应）
- UI 更新：`coordinator` → `ui.Manager`（通过 channel 发送 UICommand）
- Schema 流程：`schema.Manager` 加载 `*.schema.yaml` → `schema.Factory` 创建 Engine + Dict
- Host Render 流程：`bridge.HostRenderManager` 管理白名单进程的共享内存，`ui.Manager` 通过 `SHM.WriteFrame` 推送位图到 C++ DLL

### Testing Requirements
- 各包独立测试：`go test ./internal/...`
- `engine/pinyin/`、`dict/` 和 `schema/` 有较多单元测试，修改时务必运行
- `coordinator/input_history_test.go` 为无平台依赖的纯 Go 测试

### Common Patterns
- Windows 平台专属代码用 `_windows.go` 后缀（如 `binformat/mmap_windows.go`、`ui/dwrite_cgo_windows.go`）
- 接口定义与实现分离（如 `engine.Engine`、`bridge.MessageHandler`）
- Shadow 规则使用 pin(position) + delete 二元架构
- CGO 仅用于 DirectWrite 的 float 参数回调问题，其余 Win32 调用均通过 `syscall`/`golang.org/x/sys/windows`

## Dependencies
### Internal
- `pkg/` 下的公共包
- `internal/` 包之间有依赖关系（见各子目录）

### External
- `golang.org/x/sys/windows`
- `github.com/Microsoft/go-winio`

<!-- MANUAL: -->
