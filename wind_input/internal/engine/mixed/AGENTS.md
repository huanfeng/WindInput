<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-01 | Updated: 2026-05-15 -->

# internal/engine/mixed

## Purpose
五笔拼音混合输入引擎。内部持有独立的码表引擎（`*codetable.Engine`）和拼音引擎（`*pinyin.Engine`），根据输入长度选择查询策略，并行查询后按权重合并候选词列表。

查询策略（以最大码长 maxCodeLen=4 为例）：
- 1 码：仅查码表
- 2~4 码：并行查码表+拼音，码表优先（双向夹击权重）
- >4 码：降级为纯拼音（`IsPinyinFallback=true`）

## Key Files
| File | Description |
|------|-------------|
| `mixed.go` | `Engine`：混输引擎主体；`Config`（`MinPinyinLength`/`CodetableWeightBoost`/`ShowSourceHint`）；`ConvertEx` 核心转换逻辑（`convertCodetableOnly`/`convertMixed`/`convertPinyinOnly`）；`OnCandidateSelected` 按 `CandidateSource` 路由学习回调；`ConvertResult` 结构体（含 `IsPinyinFallback` 和拼音降级字段） |

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

#### ⚠️ 学习路由（OnCandidateSelected）与 charBuffer 连续性

`OnCandidateSelected` 按 `CandidateSource` 路由到不同子引擎，但码表的自动造词（`CodeTableAutoPhrase`）依赖连续单字 `charBuffer`，必须感知拼音输入的存在：

- **SourceCodetable**：直接路由到 `codetableEngine.OnCandidateSelected`，单字进 charBuffer，多字词触发 flush
- **SourcePinyin**：路由到 `pinyinEngine.OnCandidateSelected`；**同时**通知码表的造词策略：
  - 拼音单字 → `ls.OnWordCommitted("", text)`（code="" 可为空，flush 时由 `CalcWordCode` 重算）
  - 拼音多字词 → `codetableEngine.OnPhraseTerminated()`，终止当前单字序列触发 flush
- **default（无来源标记，如顶码/自动上屏）**：默认路由到码表，符合预期（顶码只由码表触发）

**如果不遵守上述规则**，拼音输入的字不会进入 charBuffer，导致五笔+拼音交替输入时自动造词只能看到纯五笔子序列，无法正确感知拼音边界。

#### ⚠️ 自动造词功能的历史回退原因及防范

以下问题曾多次因修改其他功能而意外回退，提交前必须验证：

1. **混输方案学习配置**：混输方案**始终**使用主方案（`PrimarySchema`）的学习配置，不维护独立配置（混输本质是主码表 + 辅助拼音）。入口在 `factory.go:createMixedEngine`（`codetableLearningSpec`）和 `manager_config.go:UpdateLearningConfig`（`codetableLS`），两处逻辑对称。**不得删除或改为条件判断**。

2. **拼音 temp layer 的 SetLimits**：混输拼音子引擎使用独立 temp layer（`schemaID="pinyin"`），它不是 `DictManager.activeStoreTemp`，`UpdateActiveTempLimits` **不会**覆盖它。凡是创建/替换拼音 temp layer 的代码路径（factory 初始化 + 热更新），都必须显式调用 `tl.SetLimits`。否则 `promoteCount=0`，`LearnWord` 永远返回 false，临时词永不晋升。

3. **日志字段 `codetableAutoPhrase`**：该字段用类型断言判断 `codetableLearning` 是否为 `*schema.CodeTableAutoPhrase`（而非 `!= nil`）。`ManualLearning{}` 也是 non-nil，若改回 `!= nil` 判断会误报 true。

### Testing Requirements
- `go test ./internal/engine/mixed/`
- `mixed_repro_test.go` 包含复现测试用例
- 新增的学习路由行为建议在 `mixed_repro_test.go` 中补充集成测试（见下文自动化测试建议）

### ⚠️ 自动化测试建议（防止学习/造词/晋升回退）

该模块的学习、造词、晋升逻辑历史上多次因其他改动意外失效，且往往不产生编译错误或崩溃，只在运行时静默失效。推荐以下测试策略：

**集成测试层**（优先，可覆盖多个组件交互）：
- 在 `mixed_repro_test.go` 中构造完整的 `Engine + DictManager + StoreTempLayer`，模拟多次 `OnCandidateSelected`，断言：
  - 达到 `promoteCount` 次后，临时词库条目消失，用户词库中出现对应词
  - 拼音来源单字上屏后，码表 charBuffer 中确实追加了该字（可通过验证最终 flush 结果来验证）
  - 拼音多字词上屏后，charBuffer 被清空（flush 结果为空）

**单元测试层**（作为补充）：
- `StoreTempLayer.LearnWord` + `PromoteWord` 的 promoteCount 边界（已有 `store_layer_test.go`）
- `CodeTableAutoPhrase.OnWordCommitted` 对单字 vs 多字词的路由
- `mixed.Engine.OnCandidateSelected` 对 SourcePinyin 的路由（验证 charBuffer 通知）

**检查清单**（每次修改学习相关代码后运行）：
```
go test ./internal/engine/mixed/...
go test ./internal/dict/...
go test ./internal/schema/...
```

### Common Patterns
- `Engine` 实现 `engine.Engine` 和 `engine.ExtendedEngine` 接口
- `GetCodetableEngine()`/`GetPinyinEngine()` 供 `engine.Manager` 访问内部引擎（用于配置热更新、学习策略注入）
- `candidate.SourceCodetable`/`candidate.SourcePinyin` 标记候选来源，供 `OnCandidateSelected` 路由

## Dependencies
### Internal
- `internal/candidate` — `Candidate`、`CandidateSource`（`SourceCodetable`/`SourcePinyin`）、`Better`
- `internal/dict` — `DictManager`、`ApplyShadowPins`
- `internal/engine/pinyin` — 拼音引擎
- `internal/engine/codetable` — 码表引擎

### External
- 无

<!-- MANUAL: -->
