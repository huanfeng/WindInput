<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-08 | Updated: 2026-04-08 -->

# internal/dict/dictcache

## Purpose
词库缓存管理。负责将文本格式的码表和字典转换为高效的二进制 `wdb` 格式，并缓存到本地（`%LOCALAPPDATA%\WindInput\cache`）。提供缓存有效性检测（文件 mtime 比较）和自动重新生成机制。已新增对 **Rime 生态**词库的完整支持：拼音（`rime_pinyin`，多文件 `.dict.yaml` 合并）和五笔（`rime_wubi`，含 import 递归发现）。

## Key Files
| File | Description |
|------|-------------|
| `cache.go` | 缓存路径管理（`GetCacheDir`、`CachePath`）和有效性检测（`NeedsRegenerate(srcPaths, wdbPath)`） |
| `convert.go` | 所有转换逻辑：`ConvertCodeTableToWdb`（传统单文件码表）、`ConvertPinyinToWdb`（Rime 拼音多文件合并）、`ConvertRimeWubiToWdb`（Rime 五笔多文件合并）、`ConvertUnigramToWdb`（Unigram 文本→wdb）；`RimePinyinSourcePaths`/`RimeWubiSourcePaths` 发现所有关联源文件；`CodeTableMeta`/`LoadCodeTableMeta` 管理 sidecar 元数据 |

## For AI Agents

### Working In This Directory
- 缓存目录：`%LOCALAPPDATA%\WindInput\cache\<name>.wdb`
- 元数据 sidecar：`<wdb_path>.meta.json`，存储码表的 Header 信息（名称、版本、码长等）
- `NeedsRegenerate(srcPaths, wdbPath)` 判断缓存是否过期（任一源文件 mtime > wdb mtime，或 wdb 不存在）
- **Rime 拼音**：`ConvertPinyinToWdb(mainDictPath, wdbPath)` 从主 `.dict.yaml` 出发，递归发现所有 `import_tables` 文件（`discoverRimePinyinFiles`），合并后写入单一 wdb
- **Rime 五笔**：`ConvertRimeWubiToWdb(mainDictPath, wdbPath)` 同理，`RimeWubiSourcePaths` 返回包含主文件和所有 import 文件的完整列表，用于 `NeedsRegenerate` 检测
- `schema/factory.go` 是主要调用方，在引擎初始化时调用各 `Convert*` 函数；失败时回退到已有 wdb 文件
- `LoadCodeTableMeta` 从 sidecar `.meta.json` 恢复码表 Header，供 `wubi.Engine.RestoreCodeTableHeader` 使用

### Testing Requirements
- 缓存有效性检测可做单元测试（文件 mtime 比较逻辑）
- 转换逻辑可通过读写往返验证（需要 Rime 词库文件）

### Common Patterns
- 词库目录内预编译的 `wubi.wdb`/`pinyin.wdb` 优先于缓存目录（`factory.go` 的加载顺序）
- Rime 格式识别：`dictType == "rime_wubi"` 或 `"rime_pinyin"` 由 Schema 文件的 `dictionaries[].type` 字段指定

## Dependencies
### Internal
- `internal/dict` — `LoadCodeTable`、`CodeTable`
- `internal/dict/binformat` — `DictWriter`、`UnigramWriter`

### External
- 无（仅标准库）

<!-- MANUAL: -->
