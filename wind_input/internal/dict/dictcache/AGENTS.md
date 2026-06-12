<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-08 | Updated: 2026-06-12 -->

# internal/dict/dictcache

## Purpose
词库缓存管理。负责将文本格式的码表和字典转换为高效的二进制 `wdb` 格式，并缓存到本地（`%LOCALAPPDATA%\WindInput\cache`）。提供缓存有效性检测（文件 mtime 比较）和自动重新生成机制。已新增对 **Rime 生态**词库的完整支持：拼音（`rime_pinyin`，多文件 `.dict.yaml` 合并）和五笔码表（`rime_codetable`，含 import 递归发现）。

**词库源格式**：由 `dictsource.go` 的格式抽象层统一处理两种磁盘格式：
- rime `.dict.yaml`：YAML 头 + TSV 数据体，单文件（**默认格式**；rime 生态交换格式，拖入即用；dictgen 也默认生成此格式）；
- split：`.dict.toml`（纯头）+ 同 stem `.dict.tsv`（制表符分隔数据体，命名约定配对）——**隐含支持，非默认生成**，可经 `cmd/dicttool split` 由 yaml 转换。

两种格式经 `OpenDictSource` 把"头来源"与"体来源"解耦为统一表示（头解析为类型化 `DictHeader`，各自原生解码；体由同一段制表符解析逻辑消费 `io.Reader`），**零行为差异**。`DictSpec.Type` 不变（`rime_codetable`/`rime_pinyin`），词库 `path` 写 `.dict.toml` 即按扩展名走 split 分支；import_tables 兄弟词库扩展名跟随主词库格式。

## Key Files
| File | Description |
|------|-------------|
| `cache.go` | 缓存路径管理（`GetCacheDir`、`CachePath`、`WdatCachePath`）和有效性检测（`NeedsRegenerate(srcPaths, wdbPath)`，命中"过期"时记 INFO 日志包含触发源文件与 mtime 差） |
| `convert.go` | 所有转换逻辑：`ConvertCodeTableToWdb`（传统单文件码表）、`ConvertRimeCodetableToWdb`（Rime 码表多文件合并，主要用于五笔）、`ConvertUnigramToWdb`（Unigram 文本→wdb）、`ConvertPinyinToWdat`（Rime 拼音多文件合并→DAT 格式，拼音唯一路径）；`RimePinyinSourcePaths`/`RimeCodetableSourcePaths` 发现所有关联源文件；`CodeTableMeta` 与 `LoadCodeTableMetaFromWdb` 处理嵌入式元数据 |
| `dict_patch.go` | `FindPatchFiles`/`LoadDictPatch`/`ApplyDictPatch`：在主词库目录下查找同名 `.dict.patch.yaml` 补丁文件并合并到转换结果（补丁统一为 YAML，主词库 `.dict.yaml`/`.dict.toml` 均推导同名 `.dict.patch.yaml`） |
| `dictsource.go` | 词库源格式抽象层：`DictHeader`（rime/split 头的统一类型化结构，双 yaml+toml tag 原生解码）、`OpenDictSource(path)→(头, 体 io.ReadCloser)`、`ReadDictHeader(path)`（仅头，供发现）、`ConvertRimeYAMLToSplit(yamlPath, outDir)`（`.dict.yaml`→`.dict.toml`+`.dict.tsv` 无损切分）；内部后缀/配对/源文件助手（`dictStem`/`dictSuffixOf`/`dictFilesFor`） |

## For AI Agents

### Working In This Directory
- 缓存目录：`%LOCALAPPDATA%\WindInput\cache\<name>.wdb`（及拼音 DAT 格式 `<name>.wdat`）
- **元数据嵌入 wdb 内部**：码表 Header（名称、版本、码长等）由 `binformat.DictWriter.SetMeta` 写入 wdb 文件的 meta 段，由 `binformat.DictReader.ReadMeta` 读取；不再使用 sidecar `.meta.json` 文件，旧 sidecar 残留可忽略
- `NeedsRegenerate(srcPaths, wdbPath)` 判断缓存是否过期（任一源文件 mtime > wdb mtime，或 wdb 不存在）；命中过期分支会写一条 INFO 日志附带触发源文件名、源/目标 mtime，便于排查"重建死循环"类问题
- **Rime 拼音**：`ConvertPinyinToWdat(mainDictPath, wdatPath)` 从主 `.dict.yaml` 出发，递归发现所有 `import_tables` 文件（`discoverRimePinyinFiles`），合并后写入单一 DAT（`.wdat`）。拼音已统一走 DAT，旧版 wdb 转换路径（`ConvertPinyinToWdb`）已移除
- **Rime 码表（五笔等）**：`ConvertRimeCodetableToWdb(mainDictPath, wdbPath)` 同理，`RimeCodetableSourcePaths` 返回包含主文件和所有 import 文件、patch 文件的完整列表，用于 `NeedsRegenerate` 检测
- **split 格式源清单**：`RimeCodetableSourcePaths`/`RimePinyinSourcePaths` 对 split 词库经 `dictFilesFor` 同时纳入 `.dict.toml` 与 `.dict.tsv`（tsv 存在才计入，保证 meta.Sources 稳定），任一变更触发重建
- **格式转换工具**：`cmd/dicttool split <a.dict.yaml ...>` 调用 `ConvertRimeYAMLToSplit` 把 rime 词库拆为 split 格式（在 `...` 处切一刀，体逐字节同构，无损）
- `schema/factory.go` 是主要调用方，在引擎初始化时调用各 `Convert*` 函数；失败时直接向上报错（不再静默回退到旧 wdb）
- `LoadCodeTableMetaFromWdb(reader)` 从 wdb 内嵌的 meta 段恢复码表 Header，供 `codetable.Engine.RestoreCodeTableHeader` 使用
- **低内存模式**：首次生成大词库（拼音 wdat / Rime 码表 wdb）峰值可超 1GB。`pkg/sysinfo.LowMemoryMode()` 为 true 时，`ConvertPinyinToWdat`/`ConvertRimeCodetableToWdb` 边转换边 `delete` 源 map 并主动 GC，且对 writer 启用 `SetLowMemory`；`ConvertCodeTableToWdb` 仅启用 writer 释放（其源 map 是 `CodeTable` 内部引用，不可 delete）。可用物理内存 < 1024MB 触发（`WINDINPUT_FORCE_LOWMEM`/`WINDINPUT_LOWMEM_MB` 可覆盖）。**仅降内存峰值、略慢，不改变输出**
- 内存峰值的另一来源是 DAT 构建（`datformat`），其 trie 节点已由 map 改为有序切片，与本模块的省内存释放协同生效

### Testing Requirements
- 缓存有效性检测可做单元测试（文件 mtime 比较逻辑）
- 转换逻辑可通过读写往返验证（需要 Rime 词库文件）

### Common Patterns
- 词库目录内预编译的 `wubi.wdb`/`pinyin.wdat` 优先于缓存目录（`factory.go` 的加载顺序）
- Rime 格式识别：`dictType == "rime_wubi"` 或 `"rime_pinyin"` 由 Schema 文件的 `dictionaries[].type` 字段指定

## Dependencies
### Internal
- `internal/dict` — `LoadCodeTable`、`CodeTable`
- `internal/dict/binformat` — `DictWriter`、`UnigramWriter`
- `internal/dict/datformat` — `WdatWriter`（拼音 DAT 写入）
- `pkg/sysinfo` — `LowMemoryMode`（低内存路径决策）

### External
- 无（仅标准库）

<!-- MANUAL: -->
