<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-20 -->

# cmd/gen_unigram

## Purpose
Unigram 语言模型频次提取工具。从 Rime 词库源文件（`.dict.yaml`）提取词语的词频数据，生成文本格式的 `unigram.txt` 文件。输出文件供 `gen_bindict` 后续生成二进制 `unigram.wdb` 使用，为拼音引擎提供词序排序。支持从多个词库文件合并词频数据（取最大值）。

## Key Files
| File | Description |
|------|-------------|
| `main.go` | 命令行入口，从 YAML 词库加载频次并输出文本文件 |

## For AI Agents

### Working In This Directory
- 命令行参数：
  - `-rime <dir>`：Rime 词库目录，包含 `8105.dict.yaml`、`base.dict.yaml`、`tencent.dict.yaml` 等文件
  - `-output <file>`：输出 unigram.txt 文件路径
- 输出文件格式：文本文件，每行 `词语\t频次`（Tab 分隔），包含注释头
- 词库加载顺序（优先级）：
  1. `8105.dict.yaml` —— 单字词频（高优先级）
  2. `base.dict.yaml` —— 基础词组
  3. `tencent.dict.yaml` —— 腾讯词向量补充
- 词频合并策略：相同词汇取最高频次值（不覆盖）
- Rime YAML 支持两种列格式：
  - 三列模式：文字\t拼音\t权重（8105.dict.yaml、base.dict.yaml）
  - 两列模式：文字\t权重（若 YAML header 中缺少 code 列）

### Testing Requirements
- 输出文件格式可通过文本检查验证
- 生成的 unigram.txt 可作为 `gen_bindict` 的输入

### Common Patterns
- 词库文件格式：Rime YAML（`.dict.yaml`）含标准 YAML header 和数据行
- YAML header 识别 `columns` 字段以判断列格式（三列或两列）
- 数据行格式检查：跳过空行、注释行、字段数不足的行
- 词频检查：忽略负数或零的频次值

## Dependencies
### Internal
- 无

### External
- 无（仅标准库）

<!-- MANUAL: -->
