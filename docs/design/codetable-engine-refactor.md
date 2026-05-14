# 码表引擎重构与标准化设计文档

## 1. 背景与痛点

当前 WindInput 的码表引擎在处理前缀匹配、词频排序和特殊词库（如极点五笔）时存在以下痛点：
1. **前缀截断与性能问题**：底层的 `LookupPrefixExcludeExact` 采用顺序字典序扫描并强行截断（Limit）。这导致输入短码时，长码常用词（如输入 `s` 时的 `sam` “模式”）可能由于排序靠后而被物理截断，永远无法显示。放开 Limit 又会导致一次性拉取数千词条，引发严重性能问题。
2. **词库风格差异导致的“杜蕾斯霸榜”**：对于按字母顺序生成的无权重词库（如某些五笔词库），引擎将文件的物理行号误认为是全局词频。开启前缀匹配时，靠前行号的生僻词（如 `sa` “杜蕾斯”）会排在靠后行号的常用词（如 `sam` “模式”）前面。
3. **过滤时机错误**：生僻字过滤和 Shadow 规则拦截发生在候选列表生成的最后阶段。如果常用字在底层拉取时因 Limit 被挤掉，顶层过滤生僻字后会导致候选列表变空。

## 2. 核心架构设计

将整个码表引擎的工作流划分为标准化的主干流程，并将特定行为抽象为可插拔/可配置的策略列表。

### 2.1 基础配置模型扩充 (`Config`)

保持对旧配置项（如 `SingleCodeInput`, `WeightAsOrder`）的向后兼容，同时引入新的策略组：

```go
type Config struct {
	// 基础配置
	LoadMode string // 加载模式: "mmap" (默认), "memory" (全内存，高性能)

	// 前缀匹配与联想
	PrefixMode     string // "none" (关闭), "sequential" (旧版), "bfs_bucket" (默认，广度优先分桶)
	AutoComplete   bool   // 精确匹配模式下空码时是否自动补全1码 (原 SingleCodeComplete)
	BucketLimit    int    // 分桶扫描时每层的候选上限，结合 IsCommon 保证不漏常用字

	// 权重与排序语义
	WeightMode        string // "global_freq" (全局权重), "inner_order" (同码内排序), "auto" (自动探测 HasWeight)
	CandidateSortMode string // "frequency" (按计算后权重排), "natural" (按文件行号排)

	// 高级修饰与特权 (默认均不开启，遵循词库作者意图)
	ProtectTopN          int    // 保护精确匹配的前 N 个词
	ShortCodeFirst       bool   // 前缀提示时，对长码施加惩罚，短码优先
	CharsetPreference    string // 字符集偏好: "none" (默认), "single_first" (单字优先), "phrase_first" (词组优先), "full_code_phrase_first" (全码词组优先)

	// 现有兼容配置...
}
```

### 2.2 广度优先分桶扫描 (BFS Bucket Scan)

彻底废弃暴力的顺序前缀扫描。
- 按剩余码长（+1码, +2码...）进行分层扫描。
- 在底层收集时引入动态回调 `isCommon(text)`，当某一层达到 `BucketLimit` 时，优先保留常用字词，丢弃生僻字。
- 保证短码常用词和长码常用词都能在可控的数量内被检索到，兼顾性能与召回率。

### 2.3 权重语义解耦 (`HasWeight` 标记)

- 在 `gen_codetable_wdb` 编译阶段，扫描整个 TXT 词库。如果所有条目都没有显式指定词频（Weight），则在 WDB 的 Meta 或 Header 中打上 `HasWeight: false` 标记。
- 引擎运行时，如果检测到 `HasWeight == false` 且配置为 `auto`，则强制将 `WeightMode` 置为 `inner_order`。
- `inner_order` 模式下，前缀匹配时会抹平跨编码的权重差异，强行回退到文件原始的 `NaturalOrder`，完美解决极点五笔的乱序问题。

### 2.4 ConvertEx 标准流水线重塑

1. **预处理与加载**：根据 `LoadMode` 选择 Mmap 或 Memory 模式。
2. **精确查找 (Phase 1)**：获取完全匹配的词条。
3. **BFS 前缀查找 (Phase 2)**：按桶深度拉取前缀候选，期间动态保留 `IsCommon`。
4. **早期过滤 (Phase 3)**：提前应用 Shadow 规则和智能生僻字过滤，避免挤占后续排序名额。
5. **权重矫正与修饰 (Phase 4)**：
   - 根据 `WeightMode` 调整权重基准。
   - 应用 `ShortCodeFirst` 梯度降权。
   - 应用 `CharsetPreference` 特权（如全码词组提前）。
6. **合并、排序与截断 (Phase 5/6)**：全局排序（`Better` 或 `BetterNatural`），应用 `ProtectTopN`，返回最终页。

## 3. 实施步骤

1. **配置层改造**：扩展 `codetable.Config`，实现向后兼容逻辑。修改 `gen_codetable_wdb` 支持 `HasWeight` 探测。
2. **底层扫描重构**：在 `binformat/reader.go` 中实现带 `IsCommon` 过滤的 `LookupPrefixBFS`。
3. **流水线重塑**：重构 `codetable.go` 中的 `ConvertEx`，串联所有新策略。
4. **全内存模式支持**：实现 `LoadMode: memory` 的数据加载逻辑，优化极致性能。
