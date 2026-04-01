<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-01 -->

# cmd/gen_bindict

## Purpose
拼音词库二进制生成工具。从 Rime 词库源文件（`8105.dict.yaml`、`base.dict.yaml`）和 Unigram 频次文件生成两个二进制词库文件：
- `pinyin.wdb`：拼音词库（主索引 + 简拼索引）
- `unigram.wdb`：Unigram 语言模型（词频数据）

这两个文件通过 mmap 加载，在运行时几乎不占堆内存。

## Key Files
| File | Description |
|------|-------------|
| `main.go` | 命令行入口，调用 `genPinyinWdb` 和 `genUnigramWdb` |

## For AI Agents

### Working In This Directory
- 命令行参数：
  - `-dict <dir>`：Rime 词库目录，包含 `8105.dict.yaml` 和 `base.dict.yaml`
  - `-unigram <file>`：Unigram 频次文件（默认 `dict/pinyin/unigram.txt`）
  - `-out <dir>`：输出目录（默认 `dict/pinyin`）
- 生成的文件格式由 `internal/dict/binformat` 定义，版本号在 `format.go` 中指定
- 词库生成过程：加载 YAML → 按 code 和 abbrev 聚合 → 权重排序 → 写入二进制文件

### Testing Requirements
- 生成的 `pinyin.wdb` 可用 `internal/dict/binformat_test.go` 验证往返一致性
- `unigram.wdb` 可通过 `internal/engine/pinyin` 的 `BinaryUnigramModel` 验证加载

### Common Patterns
- 文本词库文件格式：Rime YAML（含 code、text、weight 字段）
- 特殊权重处理（如 Unigram 转为对数概率）在 `genUnigramWdb` 中实现

## Dependencies
### Internal
- `internal/dict/binformat` — DictWriter、UnigramWriter

### External
- 无（仅标准库）

<!-- MANUAL: -->
