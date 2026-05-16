<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-08 | Updated: 2026-05-15 -->

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
| `codetable.go` | 五笔码表加载（文本格式和二进制 wdb 格式），含 `BuildReverseIndex`（全量）和 `BuildSingleCharReverseIndex`（仅单字，过滤权重 < maxWeight/10 的低频简码）；支持 Rime 词库合并结果 |
| `phrase.go` | `PhraseLayer`：短语和命令处理，支持模板变量；`SetCmdbarHook(CmdbarPhraseHook)` 装配命令直通车 hook，含 `$CC(` 或 `$CC1(` 的动态短语在 `SearchCommand` 中改走 hook 求值得到 `(display, actions)`；`SearchPrefix` 同时扫 `dynamicPhrases` 中带 cmdbar marker 的条目, 末尾用 `filterCmdbarExactOnly` 过滤掉 `$CC(` (仅精确) 条目, 保留 `$CC1(` (前缀可见); hook 报错或不可用时退化为字面量并记 WARN（不带 value 内容）。**权重 (2026-05-16 修订)**: 候选 `Weight` 由 `resolvePhraseWeight(weight)` 计算 (单参), `weight<=0 → 1000` (默认), 否则 clamp 到 `NormalizedWeightMax`; `position` 不再参与 weight 计算, 仅作 `sortPhraseCandidates` 同 code 内 tie-break (升序, 0=未调整, 已调整优先); `PhraseFileEntry.Weight` 用 `*int` 区分"未设置" vs "显式 0"。字符组 ($AA) 内字符共享 group weight, `NaturalOrder = 数组下标` 做 tie-break, 保证按 chars 字符串顺序排列 |
| `cmdbar_filter.go` | `HasCmdbarMarker(value)` 检测 `$CC(`/`$CC1(`; `IsCmdbarExactOnly(value)` 判断"仅精确匹配"语义 (含 `$CC(` 但不含 `$CC1(`); `filterCmdbarExactOnly(cs)` 给各 layer 的 `SearchPrefix` 收尾共享同一过滤语义。命令 marker 本身编码"是否前缀展开", 无需额外配置/字段 |
| `aa_marker.go` | `ParseAAMarker(value)` 解析 `$AA("name", "chars")` 字符组 marker, 返回 (groupName, chars, ok); `HasAAMarker(value)` 快速旁路判断。yaml 短语统一只用 `text` 字段表达字符组, 取代旧的 `texts`+`name` 双字段; 精确码展开为 N 个独立字符候选, 前缀显示导航候选, 语义不变。详见 docs/design/2026-05-12-command-bar-design.md §3.7 |
| `value_expand.go` | `ValueExpander` (`Hook` + `TemplateEngine`) + `ExpandResult` 统一展开任意候选 value: cmdbar marker (`$CC(`/`$CC1(`) → hook, `$X` 模板 → templateEngine, 其它 → 原样; 暴露 `HasExpandable(value)` 快速判断, 供 coordinator 候选后处理使用 |
| `shadow.go` | `ShadowLayer`：pin(position)+delete 架构——`pinned` 列表按位置固定词条，`deleted` 列表隐藏词条；YAML 序列化 |
| `user_dict.go` | `UserDict`：用户词频学习，按权重排序，持久化为 JSON |
| `adapter.go` | 引擎词库适配器（将 binformat Reader 适配为词库层） |
| `common_chars.go` | 通用规范汉字表加载：`InitCommonChars`（从默认路径加载）、`InitCommonCharsWithPath`（指定路径）；内置约 2500 个核心常用字作为 fallback；`IsCommonChar`/`IsStringCommon` 判断字符/字符串是否全部为通用规范汉字；`AddCommonChars` 运行时扩展；`ResetCommonCharsForTesting` 测试专用重置 |
| `loader.go` | 词库加载工具函数 |
| `dict.go` | 保留文件（原 Dict 接口定义，部分接口已迁移，修改前先确认引用） |
| `english_dict.go` | 英文词库：`LoadRimeDir` 自动构建/加载 `en.wdb`（mmap，不占堆），wdb 过期时从 Rime 源文件重建；`Lookup`/`LookupPrefix` 优先走 wdbReader，失败时回退 Trie |
| `trie.go` | 前缀 Trie 数据结构，供英文词库回退路径和其他组件使用 |
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
- `CodeTable.BuildReverseIndex()` 为懒加载（首次五笔反查时构建）；`BuildSingleCharReverseIndex()` 只索引单字条目并过滤权重 < maxWeight/10 的低频简码（如 cccc→晶），内存占用从 ~20-50MB 降至 ~2-3MB，为反查/代码提示的推荐路径
- 通用字符表路径：`<exeDir>/dict/common_chars.txt`

### ⚠️ StoreTempLayer.SetLimits 必须显式调用

`StoreTempLayer` 创建后 `promoteCount` 默认为 0。**`promoteCount=0` 会使 `LearnWord` 永远返回 false**（代码中有 `if l.promoteCount > 0` 守卫），导致临时词永远不会晋升到用户词库，且无任何错误或警告。

凡是通过 `NewStoreTempLayer` 或 `GetOrCreateStoreTempLayer` 获取 temp layer 并将其交给学习策略（`AutoLearning.SetTempLayer` / `CodeTableAutoPhrase.SetTempLayer`）的代码，都必须在交出前调用 `tl.SetLimits(maxEntries, promoteCount)`。

以下两条 `UpdateActiveTempLimits` **不覆盖**所有 temp layer：
- 它只更新 `activeStoreTemp`（当前方案的 dataSchemaID 对应的层）
- 混输模式下的拼音 temp layer（schemaID="pinyin"）是独立的，不是 activeStoreTemp，必须单独 SetLimits

### Testing Requirements
- 运行：`go test ./internal/dict/...`
- 测试文件：`trie_test.go`、`pinyin_dict_test.go`、`phrase_test.go`、`shadow_test.go`、`shadow_order_test.go`（pin 排序验证）、`manager_test.go`、`user_dict_freq_test.go`（词频更新）
- `binformat/binformat_test.go` 测试读写往返一致性

### Common Patterns
- 词库文件路径约定：`<exeDir>/dict/pinyin/`（拼音 Rime 格式）、`<exeDir>/dict/wubi/`（五笔 Rime 格式）
- 用户数据路径：`%APPDATA%\WindInput\`（由 `pkg/config` 定义）
- 二进制词库（mmap）优先于文本词库，几乎不占堆内存

### 短语权重档位指南 (yaml `weight` 字段, 2026-05-16 修订)

短语 / 码表 / 范化后的拼音权重值都在 `[0, 10000]` 区间, 中位 1000。短语已是独立 tier
(`mixed.PhraseWeightBoost = 1_000_000` 与 `CodetableWeightBoost = 10_000_000` 隔离),
所以同一 weight 值在三层中的实际排序位置由 tier 决定, 而不是 weight 数字大小:

| 档位 | weight 范围 | 用途 |
|---|---|---|
| 必置顶 (短语 tier 内) | 8000~10000 | signature / 公司名 / 个人 ID; 仍在 phrase tier 内, 不会越过码表 |
| 高频备选 | 4000~7000 | 短语 tier 内的常用项, 同 code 多条短语区分用 |
| 中位 (默认) | 1000 | 普通短语, 未指定 weight 时自动取值 |
| 罕用 | 200~500 | 短语 tier 内的低频项 |
| 禁用排序 | 0 | 仍可匹配, 但 weight 为 0 |

yaml 优先级 (新): `weight > 0` 显式 (clamp) | `weight <= 0` 默认 1000;
`position` 仅在同 code 多条短语 sort 时做 tie-break (升序, 0=未调整, 已调整优先于未调整),
**不再** fallback 为 `10000-position`。详见 docs/design/2026-05-16-cmdbar-followup.md §2。

混输引擎下 phrase 候选走独立 phrase tier (+1M boost), 永远 > 拼音 / < 码表词 —
短码 (1~2 字符) 输入时短语天然让位给码表常用词, 不再霸占首位。

## Dependencies
### Internal
- `internal/candidate` — Candidate 类型、`CandidateSortMode`
- `internal/dict/binformat` — 二进制文件读写
- `pkg/dictfile` — 文件格式类型（PhraseConfig、ShadowConfig、UserWord）
- `pkg/fileutil` — 原子写入

### External
- `gopkg.in/yaml.v3` — YAML 配置解析

<!-- MANUAL: -->
