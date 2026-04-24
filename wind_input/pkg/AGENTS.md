<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-20 -->

# pkg

## Purpose
公共包集合，可被 `cmd/`、`internal/` 以及外部工具（如设置应用）引用。包含配置定义、控制协议、字符编码工具、文件工具和主题系统。

## Subdirectories
| Directory | Purpose |
|-----------|---------|
| `config/` | 应用配置结构体、路径管理、运行时状态（三层加载机制） |
| `dictio/` | 字典 IO 工具库（Rime YAML、TSV、ZIP 等多格式导入/导出）(see `dictio/AGENTS.md`) |
| `encoding/` | 字符编码工具（词组编码公式解析、规则匹配、编码计算） |
| `fileutil/` | 文件工具（原子写入、安全写入、文件变更检测） |
| `rpcapi/` | JSON-RPC 协议定义和帧协议实现（服务端/客户端共用） (see `rpcapi/AGENTS.md`) |
| `systemfont/` | Windows 系统字体目录扫描和信息提供 (see `systemfont/AGENTS.md`) |
| `theme/` | 主题系统（颜色定义、主题加载、默认主题） |

## For AI Agents

### Working In This Directory
- `pkg/` 下的包可被 `internal/` 引用，但 `pkg/` 本身不得引用 `internal/`
- 添加新的公共类型时放在对应的 `pkg/` 子包，而非 `internal/`
- `pkg/config` 的结构体变更需同步更新 YAML 序列化标签和默认值

### Testing Requirements
- `pkg/theme` 有颜色解析测试（`colors_test.go`）
- `go test ./pkg/...`

### Common Patterns
- 配置文件路径：`%APPDATA%\WindInput\config.yaml`
- 数据文件路径：`%APPDATA%\WindInput\`（user_data.db 词库数据库、system.phrases.yaml 系统短语种子）

## Dependencies
### Internal
- 无（`pkg/` 是基础层）

### External
- `gopkg.in/yaml.v3`（config）

<!-- MANUAL: -->
