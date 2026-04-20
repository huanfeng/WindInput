<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-08 | Updated: 2026-04-20 -->

# internal/dict

## Purpose
词库系统核心。提供分层词库架构（Layer 模式）、多种词库类型（拼音、五笔码表、用户词典、短语、Shadow）、词库管理器（`DictManager`），以及统一查询入口 `CompositeDict`。

词库分层（优先级从高到低，LayerType 数值越小优先级越高）：
1. **PhraseLayer**（Lv0）：用户自定义短语和命令
2. **UserDict**（Lv1）：用户造词（拼音/五笔各独立）
3. **系统词库**（Lv2）：由引擎通过 Schema Factory 注册
4. **Shadow** 不参与 CompositeDict 查询，而是以 `ShadowProvider` 身份在结果排序后作呈现层覆盖

注意：原 `Dict` 接口已删除，统一使用 `CompositeDict` 作为引擎的词库查询入口。

## Key Files
| File | Description |
|------|-------------|
| `manager.go` | `DictManager`：管理 `CompositeDict`、`ShadowLayer`、`UserDict`、`PhraseLayer` 的生命周期；`RegisterSystemLayer`/`UnregisterSystemLayer` 供引擎热插拔词库层；`SwitchSchema` 切换方案时切换用户数据文件 |
| `layer.go` | `DictLayer` 接口（`Name`/`Type`/`Search`），`LayerType` 常量，`ShadowProvider` 接口 |
| `composite.go` | `CompositeDict`：按 LayerType 优先级聚合多层查询结果，持有 `ShadowProvider` 在搜索后应用 pin/delete 规则；`SetSortMode` 控制候选排序模式 |
| `pinyin_dict.go` | 拼音词库实现（基于 binformat 的 mmap 读取） |
| `codetable.go` | 五笔码表加载（文本格式和二进制 wdb 格式），含 `BuildReverseIndex`；支持 Rime 词库合并结果 |
| `phrase.go` | `PhraseLayer`：短语和命令处理，支持模板变量 |
| `shadow.go` | `ShadowLayer`：pin(position)+delete 架构——`pinned` 列表按位置固定词条，`deleted` 列表隐藏词条；YAML 序列化 |
| `user_dict.go` | `UserDict`：用户词频学习，按权重排序，持久化为 JSON |
| `adapter.go` | 引擎词库适配器（将 binformat Reader 适配为词库层） |
| `common_chars.go` | 通用规范汉字表加载：`InitCommonChars`（从默认路径加载）、`InitCommonCharsWithPath`（指定路径）；内置约 2500 个核心常用字作为 fallback；`IsCommonChar`/`IsStringCommon` 判断字符/字符串是否全部为通用规范汉字；`AddCommonChars` 运行时扩展；`ResetCommonCharsForTesting` 测试专用重置 |
| `loader.go` | 词库加载工具函数 |
| `dict.go` | 保留文件（原 Dict 接口定义，部分接口已迁移，修改前先确认引用） |
| `trie.go` | 前缀 Trie 数据结构，供拼音词库使用 |
| `temp_dict.go` | 临时词库实现，用于临时拼音模式 |
| `template.go` | 短语模板变量处理 |
| `weight_norm.go` | 权重规范化处理 |

## Subdirectories
| Directory | Purpose |
|-----------|---------|
| `binformat/` | 二进制 `.wdb` 文件格式定义、读写器、mmap 支持 |
| `dictcache/` | 码表文本格式到 wdb 的自动转换和缓存（含 Rime 生态支持） |

## For AI Agents

### Working In This Directory
- **Shadow 架构已改为 pin(position)+delete**：`pin` 操作将词条固定到指定位置（position=0 即首位），`delete` 将词条标记为隐藏；旧的 `top`/`hide` 字段不再使用
- `CompositeDict` 是引擎唯一的词库查询入口，不再有独立的 `Dict` 接口；引擎持有 `*CompositeDict` 引用
- `DictManager.RegisterSystemLayer`/`UnregisterSystemLayer` 在引擎切换时由 `engine.Manager` 调用，保证 CompositeDict 中只有当前方案的系统词库层
- `ShadowLayer` 实现 `ShadowProvider`，通过 `CompositeDict.SetShadowProvider` 注入；呈现层覆盖在搜索返回后执行
- `UserDict` 的 `Add`/`IncreaseWeight`/`Search` 方法线程安全
- `CodeTable.BuildReverseIndex()` 为懒加载（首次五笔反查时构建）
- 通用字符表路径：`<exeDir>/dict/common_chars.txt`

### Testing Requirements
- 运行：`go test ./internal/dict/...`
- 测试文件：`trie_test.go`、`pinyin_dict_test.go`、`phrase_test.go`、`shadow_test.go`、`shadow_order_test.go`（pin 排序验证）、`manager_test.go`、`user_dict_freq_test.go`（词频更新）
- `binformat/binformat_test.go` 测试读写往返一致性

### Common Patterns
- 词库文件路径约定：`<exeDir>/dict/pinyin/`（拼音 Rime 格式）、`<exeDir>/dict/wubi/`（五笔 Rime 格式）
- 用户数据路径：`%APPDATA%\WindInput\`（由 `pkg/config` 定义）
- 二进制词库（mmap）优先于文本词库，几乎不占堆内存

## Dependencies
### Internal
- `internal/candidate` — Candidate 类型、`CandidateSortMode`
- `internal/dict/binformat` — 二进制文件读写
- `pkg/dictfile` — 文件格式类型（PhraseConfig、ShadowConfig、UserWord）
- `pkg/fileutil` — 原子写入

### External
- `gopkg.in/yaml.v3` — YAML 配置解析

<!-- MANUAL: -->
