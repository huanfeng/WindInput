<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-08 | Updated: 2026-06-01 -->

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
| `composite.go` | `CompositeDict`：按 LayerType 优先级聚合多层查询结果，持有 `ShadowProvider` 在搜索后应用 pin/delete 规则；`Search`/`SearchPrefix` 接受 `SearchOptions{Limit, SortMode}`，排序模式不再由 dict 层持有，由引擎每次调用时传入（避免跨方案污染）；`SearchSystemOnly` 仅查 `LayerTypeCell+LayerTypeSystem`，供 ProtectTopN 等"只看系统码表原始顺序"的语义使用；`HasLongerCode(input)` 短路检查任一层是否存在 `code != input` 且以 input 为前缀的条目（供 codetable 引擎全码自动顶屏判定） |
| `pinyin_dict.go` | 拼音词库实现（基于 binformat 的 mmap 读取） |
| `codetable.go` | 五笔码表加载（文本格式和二进制 wdb 格式），含 `BuildReverseIndex`（全量）和 `BuildSingleCharReverseIndex`（仅单字，过滤权重 < maxWeight/10 的低频简码）；支持 Rime 词库合并结果；`HasLongerCode(input)` 检查码表中是否存在 `code != input` 且以 input 为前缀的条目（mmap 与内存模式均支持，供全码自动顶屏判定）；`AllCandidates(limit)` 返回整张码表全部候选（binReader 走 `DictReader.AllEntries`=`scanPrefix("")`，内存模式遍历 entries），供「进入即列全表」等场景（注意 `LookupPrefix("")` 在 wdb 下短路返回 nil，列全表须用 `AllCandidates`） |
| `phrase.go` | `PhraseLayer`：短语和命令处理，支持模板变量；`SetCmdbarHook(CmdbarPhraseHook)` 装配命令直通车 hook (`$CC`/`$CC1`), `SetCmdbarArrayHook(CmdbarArrayHook)` 装配字符串数组 hook (`$SS`); 含 cmdbar marker 的动态短语在 `SearchCommand` 改走 hook 求值得到 `(display, actions, modifiers)`; `SearchPrefix` 同时扫 `dynamicPhrases` 带 marker 的条目, 末尾用 `filterCmdbarExactOnly` (走 `candidateIsExactOnly` 综合 `Modifiers["prefix"]` 与字符串扫描); hook 报错时退化为字面量记 WARN (不带 value 内容)。**权重 (2026-05-16 修订)**: 候选 `Weight` 由 `resolvePhraseWeight(weight)` 计算 (单参), `weight<=0 → 1000` (默认), 否则 clamp 到 `NormalizedWeightMax`; `position` 仅作 `sortPhraseCandidates` 同 code 内 tie-break (升序, 0=未调整, 已调整优先)。**字符组 (2026-05-16)**: `PhraseGroup.Kind` 区分 `aa` (字符) / `ss` (字符串), Kind=ss 时 SearchCommand 精确码命中走 `expandSSGroup` 调 ArrayHook 运行时展开; Kind=aa 走 staticPhrases 字符级 entry; 两类共享前缀 nav 候选路径。**字符组双路径 `GroupName` 一致性约束 (2026-05-17)**: `Search` (静态精确, staticPhrases 命中且 PhraseGroup.Kind==aa) 与 `SearchCommand` (情况 2a 字符串数组 / 情况 2b 字符组精确) 两条出口的字符级 / 元素级候选都必须填 `Candidate.GroupCode + GroupName`, 否则 coordinator 的 `collapseGroupMembersIfMixed` 无法在混合场景下展示组名 nav。**SearchCommand 锁升级 (2026-05-17)**: 改 double-checked locking — 1st pass `pl.mu.RLock()` 读 cmdCache 命中即返回 (并发读不互相阻塞); 未命中升级到 `pl.mu.Lock()` + double-check + 计算 + 写 cache。`expandSSGroup` / `expandDynamicEntry` 仍要求调用方持有 W-Lock, 由 2nd pass 保证。**平台过滤 (2026-06-01)**: `PhraseFileEntry.Platform` 字段（`""`/`"all"`=全平台, `"windows"`/`"darwin"`/`"linux"`=仅该平台, 值同 `runtime.GOOS`）; `ParsePhraseYAMLFile` 经 `phraseMatchesPlatform` 按当前平台过滤, 同一 code 可写多条不同 platform 变体（如 `codl` 删行: Win=`Home/Shift+End/Backspace`, darwin=`Ctrl+A/Ctrl+K`）, 加载时只取匹配项。注意: 内置短语经 `DictManager.SeedDefaultPhrases` **仅首次** (`PhraseCount==0`) seed 进 store, `refreshSystemPhraseWeights` 只刷 weight 不刷 text — yaml 改内置短语正文/平台变体后, 旧 store 需清库才能重新 seed 生效 |
| `cmdbar_filter.go` | `HasCmdbarMarker(value)` 检测 `$CC(`/`$CC1(`; `IsExactOnly(value)` 字符串扫描语义; `candidateIsExactOnly(c)` 优先看 `c.Modifiers["prefix"]` (2026-05-16 新增, 由 cmdbar parser 在解析期填充, 含 marker syntax sugar 默认值合并), 缺失时回退字符串扫描; `filterCmdbarExactOnly(cs)` 给各 layer 的 `SearchPrefix` 收尾共享 |
| `aa_marker.go` | `ParseAAMarker(value)` 解析 `$AA("name", "chars")` 字符组 marker, 返回 (groupName, chars, ok); `HasAAMarker(value)` 快速旁路判断。yaml 短语统一只用 `text` 字段表达字符组, 取代旧的 `texts`+`name` 双字段; 精确码展开为 N 个独立字符候选, 前缀显示导航候选, 语义不变。详见 docs/design/command-bar-design.md §3.7 |
| `ss_marker.go` | `HasSSMarker(value)` / `ParseSSGroupName(value)` 处理 `$SS("name", elem, ...)` 字符串数组 marker。LoadFromStore 阶段用 ParseSSGroupName 静态提取 group name 注入 PhraseGroup; 元素 (含嵌入 `$CC`) 留待 SearchCommand 通过 `CmdbarArrayHook` 动态展开 (走 cmdbar parser/eval)。详见 docs/design/command-bar-followup.md §4.3 |
| `value_expand.go` | `ValueExpander` (`Hook` + `TemplateEngine` + `ArrayHook`) + `ExpandResult` 统一展开任意候选 value: cmdbar marker (`$CC(`/`$CC1(`) → hook, `$X` 模板 → templateEngine, 其它 → 原样; 暴露 `HasExpandable(value)` 快速判断, 供 coordinator 候选后处理使用。**`ExpandToCandidates(code, value)`** (2026-06-09): value 式把一个 raw value 展开成**一条或多条**候选 — `$AA` 走 `ParseAAMarker` 逐字符、`$SS` 走 `ArrayHook`、`$CC`/`$X` 走 `Expand` 单条、其它字面量；供引导键特殊模式(自定义码表)复用同一批底层原语，**不**改 phrase 的加载期 `$AA` 存储式展开 |
| `shadow.go` | `ShadowLayer`：pin(position)+delete 架构——`pinned` 列表按位置固定词条，`deleted` 列表隐藏词条；YAML 序列化 |
| `user_dict.go` | `UserDict`：用户词频学习，按权重排序，持久化为 JSON |
| `adapter.go` | 引擎词库适配器（将 binformat Reader 适配为词库层） |
| `common_chars.go` | 通用规范汉字表加载：`InitCommonChars`（从默认路径加载，错误吞掉）、`InitCommonCharsWithPath(path) error`（指定路径，加载失败返回 error 但仍保留内置 fallback）；内置约 189 个核心常用字作为 fallback；`IsCommonChar`/`IsStringCommon` 判断字符/字符串是否全部为通用规范汉字；`AddCommonChars` 运行时扩展；`ResetCommonCharsForTesting` 测试专用重置 |
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
- **Shadow R2 (2026-05-17) CandID 匹配**：`PinnedWord`/`DeletedWord` 新增 `CandID` 字段；`ApplyShadowPins` 匹配优先级：`rule.CandID` 非空时按 `cand.ID` 精准匹配（动态短语跨日子稳定），否则按 `rule.Word` 匹配 `cand.Text`（兼容旧规则）；`DictManager.PinWord/DeleteWord/RemoveShadowRule/HasShadowRule` 均增加 `candID string` 入参；**旧 PhraseLayer.MovePhraseUp/Down/ToTop/HasPhraseOverride/ResetPhraseOverride 已删除**，短语调位统一走 Shadow
- **HasShadowPin 拆分 (2026-05-18)**: 新增 `DictManager.HasShadowPin(code, word, candID) bool` 仅查 `rules.Pinned`, 用于右键菜单"恢复默认"启用条件 (语义: 仅恢复位置调整, 删除的候选用户在 IME 里看不到也触达不到右键菜单, 删除恢复走设置 UI); `HasShadowRule` 保留, 同时查 Pinned + Deleted, 用于设置 UI / debug 通用判断。两者共用内部辅助 `hasShadowPinMatch(rules, word, candID)`。详见 docs/design/candidate-actions.md §4
- **PhraseGroup.RawText (2026-05-18)** 保留 group 在 PhraseRecord 中的原始 Text (含 `$AA(...)` / `$SS(...)` marker), 由 `LoadFromStore` 在两个 group 注册点 (AA/SS) 填入; `SearchPrefix` / `SearchCommand` 出口的 nav 候选用 `RawText` 推导 stable id (`PhraseCandidateID(code, RawText)`), 让 Shadow pin 跨 collapse 状态稳定命中。group member (D 类型) 也填 `Candidate.GroupTemplate = group.RawText`, 让 coordinator `collapseGroupMembersIfMixed` 出来的 nav 能继承同款 id
- **同 code 多 group 支持 (2026-05-18)** `phraseGroups` 类型从 `map[string]PhraseGroup` 升级为 `map[string][]PhraseGroup`, 同 code 多个 `$AA` / `$SS` 不再覆盖。`PhraseEntry.GroupRawText` 字段让 `staticPhrases` / `dynamicPhrases` 内混杂的多 group 成员能反查归属 group。`SearchCommand` 情况 2 (精确码命中) 改为遍历所有 group, append 全部成员候选 (各自带 GroupTemplate), coordinator collapse 自动按 GroupTemplate 区分多 nav。`expandSSGroup` 函数重构为 `expandSSGroupSingle(group)`, 按指定 group 展开 (不再扫 dynamicPhrases 全部 entries)。详见 docs/design/candidate-actions.md §7.1.1
- **Nav 候选 disable 路径 (2026-05-18)**: nav 走 `DisablePhrase(code, GroupTemplate)` → `Store.SetPhraseEnabled(code, RawText, false)`, 复用普通短语的软删除路径, 不需要新增 API; **删除后无法从 IME 内"恢复默认"** (语义见上一条 HasShadowPin 拆分), 用户需去设置 UI 切回 Enabled
- **Shadow 按方案桶**：pin 和 delete 都写 `Schemas/{schemaID}/Shadow`。`StoreShadowLayer.Delete(code, word, candID)` 走 `store.DeleteShadow(schemaID, ...)`。**短语候选 delete 不走 Shadow**, 改走 `DictManager.DisablePhrase(code, text)` → `Store.SetPhraseEnabled(false)` + `ReloadPhrases()` 软删除路径; 这样设置 UI 的"启用" Switch 能反映该状态, 用户可恢复。2026-05-17 一度引入过 ShadowGlobal 全局桶承载"跨方案 delete", 后撤销 — 详见 `internal/store/shadow.go` 顶部注释
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
**不再** fallback 为 `10000-position`。详见 docs/design/command-bar-followup.md §2。

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
