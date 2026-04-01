<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-01 -->

# cmd/gen_wubi_wdb

## Purpose
五笔码表二进制转换工具。将 Rime 格式的五笔码表文件（`wubi86_jidian.dict.yaml` 等）转换为高效的二进制 `wubi.wdb` 格式。生成的二进制文件通过 mmap 加载，性能优于文本解析。

## Key Files
| File | Description |
|------|-------------|
| `main.go` | 命令行入口，调用 `dictcache.ConvertCodeTableToWdb` |

## For AI Agents

### Working In This Directory
- 命令行参数：
  - `-src <path>`：输入码表文件路径（默认 `dict/wubi86/wubi86.txt`）
  - `-out <dir>`：输出目录（默认 `dict/wubi86`）
- 输出文件：`wubi.wdb`（写入 `-out` 指定目录）
- 词库已迁移到 Rime 生态：源文件为 `dict/wubi86/wubi86_jidian.dict.yaml`（`GetWubiDictPath()` 返回此路径）
- 旧路径 `dict/wubi/wubi86.txt` 已废弃，新路径为 `dict/wubi86/`

### Testing Requirements
- 生成的 `wubi.wdb` 可用 `cmd/test_codetable` 验证查询结果
- 二进制格式由 `internal/dict/binformat` 定义

### Common Patterns
- 码表源文件格式：Rime YAML（`.dict.yaml`），含 code、text、weight 字段
- 转换逻辑在 `internal/dict/dictcache.ConvertCodeTableToWdb` 实现

## Dependencies
### Internal
- `internal/dict/dictcache` — ConvertCodeTableToWdb 函数

### External
- 无

<!-- MANUAL: -->
