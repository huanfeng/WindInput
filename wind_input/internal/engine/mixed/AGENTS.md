<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-01 | Updated: 2026-04-20 -->

# internal/engine/mixed

## Purpose
五笔拼音混合输入引擎。内部持有独立的五笔引擎（`*wubi.Engine`）和拼音引擎（`*pinyin.Engine`），根据输入长度选择查询策略，并行查询后按权重合并候选词列表。

查询策略（以五笔最大码长 maxCodeLen=4 为例）：
- 1 码：仅查五笔
- 2~4 码：并行查五笔+拼音，五笔优先（双向夹击权重）
- >4 码：降级为纯拼音（`IsPinyinFallback=true`）

## Key Files
| File | Description |
|------|-------------|
| `mixed.go` | `Engine`：混输引擎主体；`Config`（`MinPinyinLength`/`WubiWeightBoost`/`ShowSourceHint`）；`ConvertEx` 核心转换逻辑（`convertWubiOnly`/`convertMixed`/`convertPinyinFallback`）；`OnCandidateSelected` 按 `CandidateSource` 路由学习回调；`ConvertResult` 结构体（含 `IsPinyinFallback` 和拼音降级字段） |

## For AI Agents

### Working In This Directory
- **权重策略**（双向夹击）：码表精确匹配 +10M、前缀匹配 +6M；拼音纯辅音简拼按长度递减（3码 -2M，4码 -3.5M），含元音输入保持原值
- 混输模式**禁用码表顶字**（`HandleTopCode` 合法拼音序列时返回 false），超码长输入由拼音降级处理而非顶字上屏
- `SetDictManager(dm)` 在引擎创建后由 factory 调用，用于 Shadow 规则访问
- Shadow 规则在各 convert 路径末尾统一应用（幂等操作），防止合并+重排后位置偏移
- `addSourceHints`：仅在拼音候选的 `Comment` 字段添加 `"拼"` 前缀，码表候选不添加标记
- `dedupByText` 去重时保留先出现的（权重较高的）；使用 `sync.Pool` 复用 seen 映射避免 GC 压力
- `convertMixed` 内部使用 `sync.WaitGroup` 并行查询两个引擎

#### ⚠️ 顶码上屏（AutoCommitAt4）与 Shadow 的交互
混输模式下，码表子引擎设置了 `SkipShadow=true`，Shadow 由 MixedEngine 在合并后统一应用。
修改顶码或 Shadow 相关逻辑时必须注意：

- **不得直接继承** `codetableResult.ShouldCommit`：子引擎的 `checkAutoCommit` 在 Shadow 前执行，若用户通过候选调整删词，子引擎看到的候选数量仍是 Shadow 前的值，会漏判
- 应在 Shadow 应用**之后**调用 `recheckAutoCommit(input, candidates)`，从最终候选列表重新统计精确匹配数量
- `recheckAutoCommit` 判定条件：`AutoCommitAt4=true` && `len(input) >= MaxCodeLength` && 最终列表中 `Source==SourceCodetable && Code==input` 的候选恰好为 1 个
- `convertCodetableOnly` 和 `convertMixed` 均须遵守此规则；`convertPinyinOnly` 无码表自动上屏，无需处理

### Testing Requirements
- `go test ./internal/engine/mixed/`
- `mixed_repro_test.go` 包含复现测试用例

### Common Patterns
- `Engine` 实现 `engine.Engine` 和 `engine.ExtendedEngine` 接口
- `GetWubiEngine()`/`GetPinyinEngine()` 供 `engine.Manager` 访问内部引擎（用于用户词频保存、引擎信息展示）
- `candidate.SourceWubi`/`candidate.SourcePinyin` 标记候选来源，供 `OnCandidateSelected` 路由

## Dependencies
### Internal
- `internal/candidate` — `Candidate`、`CandidateSource`（`SourceWubi`/`SourcePinyin`）、`Better`
- `internal/dict` — `DictManager`、`ApplyShadowPins`
- `internal/engine/pinyin` — 拼音引擎
- `internal/engine/wubi` — 五笔引擎

### External
- 无

<!-- MANUAL: -->
