<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-01 -->

# cmd/gen_unigram

## Purpose
Unigram 频次提取工具。从 Rime 词库源文件（`.dict.yaml`）提取词语的真实词频，生成文本格式的 `unigram.txt` 文件，供 `gen_bindict` 后续生成二进制 `unigram.wdb` 使用。

## Key Files
| File | Description |
|------|-------------|
| `main.go` | 命令行入口，从 YAML 词库加载频次并输出文本文件 |

## For AI Agents

### Working In This Directory
- 命令行参数：
  - `-rime <dir>`：Rime 词库目录，包含 `8105.dict.yaml`、`base.dict.yaml` 等文件
  - `-output <file>`：输出 unigram.txt 文件路径
- 输出文件格式：行文本，每行 `词语\t频次`（Tab 分隔）
- 词频来源：Rime YAML 中的权重字段（通常是对数概率或频次值）
- 输出文件包含注释头（以 `#` 开头）

### Testing Requirements
- 输出文件格式可通过文本检查验证
- 生成的 unigram.txt 可作为 `gen_bindict` 的输入

### Common Patterns
- 词库文件格式：Rime YAML，含 text 和 weight 字段
- 频次聚合：相同词汇的频次取最大值或求和（具体逻辑在源码实现）

## Dependencies
### Internal
- 无

### External
- 无（仅标准库）

<!-- MANUAL: -->
