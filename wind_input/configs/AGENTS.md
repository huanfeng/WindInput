<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# configs

## Purpose
示例配置文件目录。这些文件是用户数据文件的模板，不会在运行时被程序直接读取。实际配置文件位于 `%APPDATA%\WindInput\`。

## Key Files
| File | Description |
|------|-------------|
| `phrases.example.yaml` | 特殊短语配置示例，包含简单短语、模板变量、多候选、内置命令 |
| `shadow.example.yaml` | Shadow 规则配置示例，用于置顶、删除、调整词条权重 |

## Subdirectories
（无）

## For AI Agents

### Working In This Directory
- 修改示例文件时需确保与 `pkg/dictfile` 中的数据结构保持一致
- `phrases.yaml` 支持模板变量：`{year}` `{month}` `{day}` `{hour}` `{minute}` `{second}` `{week}`
- `phrases.yaml` 支持内置命令：`date` `time` `datetime` `week` `uuid` `timestamp`
- Shadow 规则的 `action` 值：`top`（置顶）、`delete`（隐藏）、`reweight`（调权重）

### Testing Requirements
- 示例文件格式可用 `go test ./internal/dict/...` 间接验证（加载逻辑在 dict 包）

### Common Patterns
- YAML 格式，UTF-8 编码
- 实际用户文件复制自这些示例并按需修改

## Dependencies
### Internal
- `pkg/dictfile` — 定义 `PhrasesConfig` 和 `ShadowConfig` 数据结构

### External
- 无

<!-- MANUAL: -->
