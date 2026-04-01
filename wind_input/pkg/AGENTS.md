<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-01 -->

# pkg

## Purpose
公共包集合，可被 `cmd/`、`internal/` 以及外部工具（如设置应用）引用。包含配置定义、控制协议、词库文件格式类型、字符编码工具、文件工具和主题系统。

## Subdirectories
| Directory | Purpose |
|-----------|---------|
| `config/` | 应用配置结构体、路径管理、运行时状态（三层加载机制） |
| `control/` | 控制管道通信协议（客户端 + 协议定义） |
| `dictfile/` | 词库文件数据类型（短语、Shadow、用户词）及读写操作 |
| `encoding/` | 字符编码工具（词组编码公式解析、规则匹配、编码计算） |
| `fileutil/` | 文件工具（原子写入、安全写入、文件变更检测） |
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
- 数据文件路径：`%APPDATA%\WindInput\`（phrases.yaml、shadow.yaml、用户词库）

## Dependencies
### Internal
- 无（`pkg/` 是基础层）

### External
- `gopkg.in/yaml.v3`（config、dictfile）

<!-- MANUAL: -->
