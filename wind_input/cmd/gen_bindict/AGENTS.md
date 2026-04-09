<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-08 -->

# cmd/gen_bindict

## Purpose
拼音词库二进制生成工具。从 Rime 词库源文件（`8105.dict.yaml`、`base.dict.yaml`）和 Unigram 频次文件生成两个二进制词库文件，供拼音引擎使用：
- `pinyin.wdb`：拼音词库（code 主索引 + abbrev 简拼索引），包含词文本和权重
- `unigram.wdb`：Unigram 语言模型（词频数据，存储对数概率）

这两个文件通过 mmap 加载，在运行时几乎不占堆内存。

## Key Files
| File | Description |
|------|-------------|
| `main.go` | 命令行入口，调用 `genPinyinWdb` 和 `genUnigramWdb` |

## For AI Agents

### Working In This Directory
- 命令行参数：
  - `-dict <dir>`：Rime 词库目录，包含 `8105.dict.yaml` 和 `base.dict.yaml`（默认 `schemas/pinyin`）
  - `-unigram <file>`：Unigram 频次文件（默认 `schemas/pinyin/unigram.txt`）
  - `-out <dir>`：输出目录（默认 `schemas/pinyin`）
- 生成的文件格式由 `internal/dict/binformat` 定义
- 词库生成过程：
  1. 加载 YAML 词库文件，解析 `文本\t拼音\t权重` 三列格式
  2. 按 code（拼音去空格）聚合条目，对于 2 字及以上词语构建简拼（abbrev）索引
  3. 按权重降序排列候选词
  4. 调用 `DictWriter.AddCode()` 和 `DictWriter.AddAbbrev()` 添加条目
  5. 调用 `writer.Write()` 写入二进制文件

### Testing Requirements
- 生成的 `pinyin.wdb` 和 `unigram.wdb` 可通过拼音引擎加载验证
- 词库大小检查：通常 `pinyin.wdb` 在 10-50MB 范围
- 可用 `internal/engine/pinyin` 的 Lookup 和 LookupAbbrev 验证检索功能

### Common Patterns
- 文本词库文件格式：Rime YAML（含 code、text、weight 字段）
- 特殊权重处理（如 Unigram 转为对数概率）在 `genUnigramWdb` 中实现

## Dependencies
### Internal
- `internal/dict/binformat` — DictWriter、UnigramWriter

### External
- 无（仅标准库）

<!-- MANUAL: -->
