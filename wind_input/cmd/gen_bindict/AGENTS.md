<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-20 -->

# cmd/gen_bindict

## Purpose
Unigram 语言模型二进制生成工具。从 Unigram 频次文件生成供拼音引擎使用的二进制文件：
- `unigram.wdb`：Unigram 语言模型（词频数据，存储对数概率）

该文件通过 mmap 加载，在运行时几乎不占堆内存。

> 注：拼音词库（原 `pinyin.wdb`）已统一改用 DAT（`.wdat`）格式，由运行时按需调用 `dictcache.ConvertPinyinToWdat` 构建，不再由本工具离线生成。

## Key Files
| File | Description |
|------|-------------|
| `main.go` | 命令行入口，调用 `genUnigramWdb` |

## For AI Agents

### Working In This Directory
- 命令行参数：
  - `-unigram <file>`：Unigram 频次文件（默认 `schemas/pinyin/unigram.txt`）
  - `-out <dir>`：输出目录（默认 `schemas/pinyin`）
- 生成的文件格式由 `internal/dict/binformat` 定义
- Unigram 生成过程：
  1. 加载 `文本\t频次` 两列格式的频次文件
  2. 将频次归一化为对数概率（`log(freq/total)`）
  3. 调用 `UnigramWriter.Add()` 添加条目
  4. 调用 `writer.Write()` 写入二进制文件

### Testing Requirements
- 生成的 `unigram.wdb` 可通过拼音引擎加载验证
- 可用 `internal/engine/pinyin` 的语言模型打分验证

### Common Patterns
- 频次文件格式：`文本\t频次` 两列
- 特殊权重处理（频次转为对数概率）在 `genUnigramWdb` 中实现

## Dependencies
### Internal
- `internal/dict/binformat` — DictWriter、UnigramWriter

### External
- 无（仅标准库）

<!-- MANUAL: -->
