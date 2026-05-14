<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-08 | Updated: 2026-05-14 -->

# internal/dict/dictcache

## Purpose
词库缓存管理。负责将文本格式的码表和字典转换为高效的二进制 `wdb` 格式，并缓存到本地（`%LOCALAPPDATA%\WindInput\cache`）。提供缓存有效性检测（文件 mtime 比较）和自动重新生成机制。已新增对 **Rime 生态**词库的完整支持：拼音（`rime_pinyin`，多文件 `.dict.yaml` 合并）和五笔码表（`rime_codetable`，含 import 递归发现）。

## Key Files
| File | Description |
|------|-------------|
| `cache.go` | 缓存路径管理（`GetCacheDir`、`CachePath`、`WdatCachePath`）和有效性检测（`NeedsRegenerate(srcPaths, wdbPath)`，命中"过期"时记 INFO 日志包含触发源文件与 mtime 差） |
| `convert.go` | 所有转换逻辑：`ConvertCodeTableToWdb`（传统单文件码表）、`ConvertPinyinToWdb`（Rime 拼音多文件合并）、`ConvertRimeCodetableToWdb`（Rime 码表多文件合并，主要用于五笔）、`ConvertUnigramToWdb`（Unigram 文本→wdb）、`ConvertPinyinToWdat`（拼音 DAT 格式）；`RimePinyinSourcePaths`/`RimeCodetableSourcePaths` 发现所有关联源文件；`CodeTableMeta` 与 `LoadCodeTableMetaFromWdb` 处理嵌入式元数据 |
| `dict_patch.go` | `FindPatchFiles`/`LoadDictPatch`/`ApplyDictPatch`：在主词库目录下查找同名 `.dict.patch.yaml` 补丁文件并合并到转换结果 |

## For AI Agents

### Working In This Directory
- 缓存目录：`%LOCALAPPDATA%\WindInput\cache\<name>.wdb`（及拼音 DAT 格式 `<name>.wdat`）
- **元数据嵌入 wdb 内部**：码表 Header（名称、版本、码长等）由 `binformat.DictWriter.SetMeta` 写入 wdb 文件的 meta 段，由 `binformat.DictReader.ReadMeta` 读取；不再使用 sidecar `.meta.json` 文件，旧 sidecar 残留可忽略
- `NeedsRegenerate(srcPaths, wdbPath)` 判断缓存是否过期（任一源文件 mtime > wdb mtime，或 wdb 不存在）；命中过期分支会写一条 INFO 日志附带触发源文件名、源/目标 mtime，便于排查"重建死循环"类问题
- **Rime 拼音**：`ConvertPinyinToWdb(mainDictPath, wdbPath)` 从主 `.dict.yaml` 出发，递归发现所有 `import_tables` 文件（`discoverRimePinyinFiles`），合并后写入单一 wdb
- **Rime 码表（五笔等）**：`ConvertRimeCodetableToWdb(mainDictPath, wdbPath)` 同理，`RimeCodetableSourcePaths` 返回包含主文件和所有 import 文件、patch 文件的完整列表，用于 `NeedsRegenerate` 检测
- `schema/factory.go` 是主要调用方，在引擎初始化时调用各 `Convert*` 函数；失败时直接向上报错（不再静默回退到旧 wdb）
- `LoadCodeTableMetaFromWdb(reader)` 从 wdb 内嵌的 meta 段恢复码表 Header，供 `codetable.Engine.RestoreCodeTableHeader` 使用

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
