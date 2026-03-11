<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# pkg/dictfile

## Purpose
词库文件数据类型定义和读写逻辑。定义 `phrases.yaml`、`shadow.yaml`、用户词库文件的数据结构，供服务端（`internal/dict`）和外部工具共用。

## Key Files
| File | Description |
|------|-------------|
| `types.go` | 核心类型：`PhraseEntry`、`PhraseConfig`、`PhrasesConfig`、`ShadowAction`、`ShadowRuleConfig`、`ShadowConfig`、`UserWord`、`UserDictData` |
| `phrase.go` | `PhrasesConfig` 的 YAML 读写函数 |
| `shadow.go` | `ShadowConfig` 的 YAML 读写函数 |
| `userdict.go` | `UserDictData` 的 JSON 读写函数 |

## For AI Agents

### Working In This Directory
- `PhraseConfig.Type` 值：空字符串（普通短语）或 `"command"`（内置命令）
- `PhraseConfig.Handler` 内置命令名：`date`、`time`、`datetime`、`week`、`uuid`、`timestamp`
- `ShadowAction` 枚举：`top`（置顶）、`delete`（隐藏）、`reweight`（调整权重）
- `UserDictData` 使用 JSON 格式（非 YAML），路径在 `pkg/config` 中定义
- 短语模板变量（在 `internal/dict/phrase.go` 中展开）：`{year}`、`{month}`、`{day}`、`{hour}`、`{minute}`、`{second}`、`{week}`

### Testing Requirements
- 纯 Go 逻辑，可做序列化往返单元测试

### Common Patterns
- 文件格式示例见 `configs/phrases.example.yaml` 和 `configs/shadow.example.yaml`
- 用户词库采用 JSON 而非 YAML，便于程序读写（YAML 适合人工编辑）

## Dependencies
### Internal
- 无

### External
- `gopkg.in/yaml.v3` — 短语和 Shadow 文件的 YAML 解析

<!-- MANUAL: -->
