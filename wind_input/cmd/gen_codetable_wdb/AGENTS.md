<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-20 -->

# cmd/gen_codetable_wdb

## Purpose
码表二进制转换工具。将各类输入法码表文件（五笔、郑码、仓颉等）转换为高效的二进制 `codetable.wdb` 格式。生成的二进制文件通过 mmap 加载，性能优于文本解析。支持 Rime 格式的 `.dict.yaml` 文件。

## Key Files
| File | Description |
|------|-------------|
| `main.go` | 命令行入口，调用 `dictcache.ConvertCodeTableToWdb` |

## For AI Agents

### Working In This Directory
- 命令行参数：
  - `-src <path>`：输入码表文件路径（默认 `schemas/wubi86/wubi86.txt`）
  - `-out <dir>`：输出目录（默认 `schemas/wubi86`）
- 输出文件：`codetable.wdb`（写入 `-out` 指定目录）
- 码表转换由 `internal/dict/dictcache.ConvertCodeTableToWdb()` 实现
- 支持 Rime 格式码表文件，包含 `text`、`code`、`weight` 字段

### Testing Requirements
- 生成的 `wubi.wdb` 可用 `cmd/test_codetable` 验证查询结果
- 二进制格式由 `internal/dict/binformat` 定义

### Common Patterns
- 码表源文件格式：Rime YAML（`.dict.yaml`）或纯文本，含 code、text、weight 字段
- 转换流程：加载源文件 → 构建码表数据结构 → 按 code 聚合候选词 → 按权重排序 → 写入二进制格式

## Dependencies
### Internal
- `internal/dict/dictcache` — ConvertCodeTableToWdb 函数

### External
- 无

<!-- MANUAL: -->
