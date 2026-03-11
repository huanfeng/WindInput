<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# internal/dict

## Purpose
词库系统核心。提供分层词库架构（Layer 模式）、多种词库类型（拼音、五笔码表、用户词典、短语、Shadow）、词库管理器（`DictManager`），以及供引擎使用的 `Dict` 接口。

词库分层（优先级从高到低）：
1. **PhraseLayer**（Lv1）：用户自定义短语和命令
2. **ShadowLayer**（Lv2）：置顶/删除规则覆盖
3. **UserDict**（Lv3）：用户造词（拼音/五笔各独立）
4. **系统词库**（Lv4）：由引擎加载后注册

## Key Files
| File | Description |
|------|-------------|
| `dict.go` | `Dict`、`PrefixSearchable`、`AbbrevSearchable`、`CommandSearchable` 接口定义 |
| `manager.go` | `DictManager`：统一管理所有词库层的加载、保存、切换和生命周期 |
| `layer.go` | `DictLayer` 接口，各层实现的基础 |
| `composite.go` | `CompositeDict`：聚合多个词库层，按优先级合并搜索结果 |
| `trie.go` | 前缀 Trie 数据结构，供拼音词库使用 |
| `pinyin_dict.go` | 拼音词库实现（基于 binformat 的 mmap 读取） |
| `codetable.go` | 五笔码表加载（文本格式和二进制 wdb 格式），含 `BuildReverseIndex` |
| `phrase.go` | `PhraseLayer`：短语和命令处理，支持模板变量 |
| `shadow.go` | `ShadowLayer`：置顶/删除规则的读写和应用 |
| `user_dict.go` | `UserDict`：用户词频学习，按权重排序，持久化为 JSON |
| `adapter.go` | 引擎词库适配器（将 binformat Reader 适配为 Dict 接口） |
| `common_chars.go` | 通用规范汉字表加载（`InitCommonCharsWithPath`） |
| `loader.go` | 词库加载工具函数 |
| `dictcache/` | 词库缓存目录（文本码表 → wdb 的转换和缓存管理） |
| `binformat/` | 二进制词库文件格式（mmap 读写） |

## Subdirectories
| Directory | Purpose |
|-----------|---------|
| `binformat/` | 二进制 `.wdb` 文件格式定义、读写器、mmap 支持 |
| `dictcache/` | 码表文本格式到 wdb 的自动转换和缓存 |

## For AI Agents

### Working In This Directory
- `DictManager.SetActiveEngine(engineType)` 切换活跃用户词库（拼音/五笔各独立）
- 系统词库由引擎初始化后通过 `RegisterSystemLayer` 注册，引擎切换时 `UnregisterSystemLayer`
- Shadow 层不参与 `CompositeDict` 的搜索，而是作为规则提供者过滤其他层的结果
- `UserDict` 的 `Add`/`IncreaseWeight`/`Search` 方法是线程安全的
- `CodeTable.BuildReverseIndex()` 为懒加载（首次查询时构建），用于五笔反查提示
- 通用字符表路径：`<exeDir>/dict/common_chars.txt`

### Testing Requirements
- 运行：`go test ./internal/dict/...`
- 测试文件：`trie_test.go`、`pinyin_dict_test.go`、`phrase_test.go`、`shadow_test.go`
- `binformat/binformat_test.go` 测试读写往返一致性

### Common Patterns
- 词库文件路径约定：`<exeDir>/dict/pinyin/`（拼音）、`<exeDir>/dict/wubi/wubi86.txt`（五笔）
- 用户数据路径：`%APPDATA%\WindInput\`（由 `pkg/config` 定义）
- 二进制词库优先于文本词库（性能更好，mmap 几乎不占堆内存）

## Dependencies
### Internal
- `internal/candidate` — Candidate 类型
- `internal/dict/binformat` — 二进制文件读写
- `pkg/dictfile` — 文件格式类型（PhraseConfig、ShadowConfig、UserWord）
- `pkg/fileutil` — 原子写入

### External
- `gopkg.in/yaml.v3` — YAML 配置解析

<!-- MANUAL: -->
