# 拼音候选质量优化

## 概述

本文档记录拼音引擎候选生成过程中发现的质量问题及其修复方案。

## 问题 1：模糊音配置热更新不生效

### 现象
用户在设置中关闭模糊音后保存，输入法仍然按模糊音匹配（如 `linwai` 输出"另外"）。

### 根因
1. **混输方案未处理**：`reload_handler.go` 的 `reloadActiveSchemaConfig()` 中，switch 语句只处理了 `EngineTypePinyin` 和 `EngineTypeCodeTable`，缺少 `EngineTypeMixed` 分支。用户使用的五笔拼音（`wubi86_pinyin`）是混输方案，配置保存后 `UpdatePinyinOptions` 不会被调用。
2. **线程安全缺失**：`Engine.config.Fuzzy` 的读写无同步机制，热更新时存在数据竞争。
3. **字段映射不完整**：`FuzzyConfig` 的 `IanIang` 和 `UanUang` 在 3 处配置映射中遗漏。

### 修复
- 新增 `EngineTypeMixed` case，从次方案获取拼音配置
- `Engine` 新增 `fuzzyPtr atomic.Pointer[FuzzyConfig]`，读写通过原子操作
- 补全所有配置映射中的 `IanIang`/`UanUang` 字段

## 问题 2：候选词排序不稳定

### 现象
同一输入（如 `linwai`）多次查询，候选顺序不确定（如"林外/临外/林歪"顺序变化）。

### 根因
三层叠加：
1. **Map 迭代随机**：候选词先存入 `map[string]*Candidate`，Go 的 map 遍历顺序不确定
2. **非稳定排序**：多处使用 `sort.Slice`，对相等元素不保证顺序
3. **比较函数非全序**：`Better()` 在 Weight/Code/NaturalOrder/ConsumedLength 都相同时无法区分不同文本的候选

### 修复
- `Better()` 增加 `Text` 最终兜底比较，确保全序关系
- 关键排序路径统一改用 `sort.SliceStable`

## 问题 3：Viterbi 造句产生高频单字拼凑伪词组

### 现象
输入 `qinta` 出现"前他"、`linwai` 出现"林歪"等词库中不存在的单字组合。

### 根因
Viterbi 造句在找不到多字词路径时，退化为高频单字组合：
1. **Lattice 单字回退无惩罚**：`BuildLattice` 为确保通路，为每个音节添加单字节点，但 LogProb 与正常词库词完全一样
2. **Bigram 回退无惩罚**：`BigramModel.LogProb` 找不到词对时直接返回 Unigram 概率，高频单字组合得分可超过低频真实词组
3. **无过滤机制**：纯单字拼凑的 Viterbi 结果直接进入候选列表

### 修复
- **过滤**：短输入（≤3 音节）的纯单字 Viterbi 结果直接丢弃
- **Lattice 惩罚**：单字回退节点施加 `singleCharPenalty = -3.0`
- **Bigram 回退惩罚**：未命中词对时施加 `backoffPenalty = -4.0`
- **长句兜底**：≥4 音节保留单字回退，但降低 `initialQuality`

## 问题 4：非首音节单字候选导致输入丢失

### 现象
输入 `linwai` 时，候选末尾出现"外/歪/崴"（仅对应第二音节 `wai`），选中后 `lin` 被丢弃。

### 根因
`convertCore` 步骤4中，非首音节单字（如 `wai→外`）被加入初始候选列表，`ConsumedLength` 设为消耗到该音节结束位置（整个 `linwai`），但文本只有一个字。上屏时引擎认为全部输入已处理完毕，前面未确认的音节被丢弃。

### 修复
移除非首音节单字候选的生成。这些候选应在用户部分上屏（确认首音节）后自然出现，不需要在初始列表中预生成。

## 问题 5：DAG 贪心切分导致音节丢失

### 现象
输入 `henihejiele` 时，DAG 贪心选择最长匹配 "hen"，导致后续 "i" 无法匹配，"ni" 被拆散。实际应切分为 "he"+"ni"+"he"+"jie"+"le" 以覆盖全部输入。

### 根因
`MaximumMatch()` 使用正向贪心——每步选当前位置最长音节。这在存在歧义时无法回溯：`hen`(3字符) > `he`(2字符)，但选了 `hen` 后 `ihejiele` 的 `i` 不是有效音节起点，后续覆盖率反而更低。

### 修复
将 `MaximumMatch()` 从贪心改为 DP 全局最优：
- `dp[i]` 记录从位置 0 到位置 i 的最大覆盖字符数
- 遍历所有 DAG 节点更新最优路径
- 回溯构建最终音节序列

同时 `Parser.parseSegment` 的余部处理也增加 DP 回退，避免贪心 `MatchPrefixAt` 产生歧义切分。

## 问题 6：前导 partial 音节导致候选消耗不匹配

### 现象
输入 `lwai`（l 为 partial，wai 为 exact）时：
1. 步骤 4a 的 `dict.Lookup("li")` 返回"立案"等多字词，但 `ConsumedLength` 只设为 1（partial "l" 的长度），消耗与候选不匹配
2. 简拼模式下 "lw" 匹配"龙王"（long+wang），但实际输入的 "wai" ≠ "w"，选中后 "ai" 丢失

### 根因
1. **步骤 4a/4b 未过滤多字词**：注释写"单字候选"但 `Lookup` 返回所有条目
2. **简拼匹配未区分纯 partial**：混合输入（partial+exact）中全拼音节的首字母不应被当作简拼

### 修复
1. 步骤 4a/4b 增加 `charCount != 1` 过滤，仅保留单字候选
2. 简拼匹配增加 `isPureAbbrev` 守卫：`syllableCount == 0 && len(allSyllables) >= 2`，仅在所有音节都是 partial 时启用

## 问题 7：ContiguousCompleted 防护体系

### 背景
问题 4-6 的根源是同一个：候选的 `ConsumedLength` 跨过了输入中未被候选覆盖的音节（partial 间隔），导致用户选词后这些音节被静默丢弃。

### 设计
引入 `ContiguousCompletedFromStart()` 方法：从输入起始位置返回连续的完成音节（遇到 partial 立即中断）。

```
"nihao"  → [ni(E),hao(E)]       → contiguous=["ni","hao"], end=5
"nihdao" → [ni(E),h(P),dao(E)]  → contiguous=["ni"],       end=2
"lwai"   → [l(P),wai(E)]        → contiguous=[],            end=0
```

所有候选生成步骤（0b/1/1b/2/3/4/5/6）统一使用 `contiguousCount`/`contiguousSyllables` 替代 `syllableCount`/`completedSyllables`：

| 步骤 | 触发条件 | 说明 |
|------|---------|------|
| 0b Viterbi | `contiguousCount >= 2` | 仅对连续完成音节造句 |
| 1 精确匹配 | `contiguousCount > 0` | 仅匹配连续音节编码 |
| 1b 多切分 | `contiguousCount > 0` | 同上 |
| 2 子词组 | `contiguousCount > 1` | 传入 contiguousSyllables |
| 3 前缀匹配 | `contiguousCount > 0` | 同上 |
| 4 首音节单字 | `contiguousCount > 0` | 首连续音节 |
| 4a leading partial | `contiguousCount == 0 && syllableCount > 0` | 首段 partial 展开 |
| 4b 多 partial 首字 | `contiguousCount == 0 && len(allSyllables) > 1` | 纯 partial 首字 |
| 5 partial 前缀 | `partial != "" && (contiguousCount > 0 \|\| single)` | 尾部 partial 安全条件 |
| 6 简拼 | `syllableCount == 0 && len(allSyllables) >= 2` | 纯 partial 简拼 |

### 子词组 ConsumedLength 精确化

原逻辑中非首位子词组（start>0）的 `ConsumedLength = len(parsed.Input)` 消耗整个输入。修正为 `ConsumedBytesForCompletedN(start + length)`——精确到子词组最后一个音节的结束位置。非首位子词组的 `initialQuality` 降级为 2.0（首位为 3.0）。

## 问题 8：多字词 Unigram 估算缺失

### 现象
`ruguo` 首候选为"入锅"而非"如果"。Viterbi 中词库合法词组（如"和解"）被单字组合碾压。

### 根因
多字词不在 Unigram 模型中时直接返回 `minProb`（极低值），导致所有未登录多字词在 Viterbi 路径比较中完败于高频单字组合。

### 修复
- `UnigramModel.LogProb`：多字词 fallback 改用 `CharBasedScore`（字符平均 LogProb），常见字组成的词得分更高
- `BinaryUnigramModel.LogProb`：同步应用 `CharBasedScore` fallback
- `Lattice.calcLogProb`：多字词不在 Unigram 中时使用 `CharBasedScore` 替代原始权重归一化
- `BigramModel` 回退惩罚从 `-4.0` 调整为 `-1.0`，避免过度惩罚

## 问题 9：非首位子词组导致输入丢失

### 现象
输入 `hejiele` 时，候选中出现"接了"（start=1，对应 jie+le），选中后 `he` 被静默丢弃。
输入 `nizhibuzhidao` 时，"知道"（start=3）、"不知道"（start=2）等非首位子词组的 `ConsumedLength` 包含前面不属于它们的音节。

### 根因
`lookupSubPhrasesEx` 枚举所有位置（start=0..n）的子词组，非首位子词组的 `ConsumedLength` 从位置 0 开始计算，会吞掉前面的音节。

### 修复
`lookupSubPhrasesEx` 只生成 start=0 的子词组。非首位词组由 Viterbi 组句提供（如"和解了"拆为"和解"+"了"），不需要独立候选。

## 问题 10：Viterbi 组句中虚词组合排名偏高

### 现象
输入 `hejiele` 时，Viterbi 造句结果"和接了"排在"和解了"前面。原因是"接了"虽不是高频词，但"接"和"了"作为高频单字在 Bigram 模型中得分很高。

### 根因
Lattice 构建时，单字回退节点无差异化惩罚，高频虚词（了/的/着/过）和实义词（接/街/借）同等对待。

### 修复
- **虚词白名单差异化惩罚**：`singleCharPenalty` 从统一 -3.0 改为：虚词（了/的/我/你/不/是等 50+ 高频功能词）仅 -0.5，普通单字保持 -3.0
- **实义词加分**：unigram 模型中存在的多字实义词（如"和解"）获得 +1.5 bonus
- **V+助词惩罚**：以"了/的/着/过/得/地"结尾的多字词（如"接了"）额外 -1.0 惩罚
- **charBasedPenalty**：不在 unigram 中的多字词额外 -2.0，区分估算频率与实际观测频率

## 问题 11：完整音节前缀预测产生超范围候选

### 现象
输入 `ruguo` 首候选为"如果"，但翻页出现"如果爱"、"如果把"等超出输入音节数的词组。

### 根因
步骤 3 对已完成音节做 `LookupPrefix`，匹配到以 `ruguo` 开头的所有编码，包括 `ruguoai`（如果爱）、`ruguoba`（如果把）等超出输入的词组。

### 修复
移除步骤 3。尾部 partial 的前缀匹配由步骤 5 处理，步骤 5 已有 `charCount > totalSyllableCount` 过滤，不会产生超范围候选。

## 问题 12：Viterbi 造句输出过多导致长句溢出

### 现象
长句输入时出现 3 个很长的造句候选，占据大量候选栏空间。主流输入法通常只保留 1 个最优长句。

### 修复
`ViterbiTopK` 参数从 3 改为 1，只保留最优路径。

## 问题 13：退格键行为与主流输入法不一致

### 现象
已有部分确认（如"我"已上屏，剩余 `buzhidao`），按退格键删除的是 `buzhidao` 末尾字符而非撤销"我"的确认。

### 修复
`handleBackspace` 优先检查 `confirmedSegments`：有确认段时弹出最后一段并将其编码回填到缓冲区前端，而非删除缓冲区末尾字符。

## 问题 14：前缀匹配的全覆盖词组排在单字后面

### 现象
输入 `rug`（ru + partial g），首候选是单字"如"而非词组"如果"。主流输入法中覆盖完整输入的词组应排首位。

### 根因
步骤 5 全覆盖词组（charCount >= totalSyllableCount）的 `initialQuality=3.0`、`coverage=syllableCount/total`，低于步骤 1 单字的 `iq=4.0`。评分公式 `exp(nw) + iq + coverage` 中 iq 差距 1.0 无法被 exp(nw) 弥补。

### 修复
步骤 5 全覆盖词组提升为 `iq=4.0`（与精确匹配同级）、`coverage=1.0`（视为完全覆盖输入），使词组 score 高于单字 0.5 分。

## 问题 15：生产环境 LM 评分完全失效

### 现象
所有同类候选（如 `rug` 下的如歌/入宫/乳沟）权重完全相同（6000000），unigram 语言模型未生效。

### 根因
`schema/factory.go` 通过 `engine.SetUnigram(bm)` 加载 unigram 模型，但 `SetUnigram` 只设置了 `e.unigram` 字段，未同步更新 `rimeScorer`。`rimeScorer` 在引擎构造时以 `NewRimeScorer(nil, nil)` 初始化，之后从未被更新，导致 `ScoreWithLM` 中 `s.unigram == nil`，LM 加成被跳过。

注意：`LoadUnigram`（仅在测试中使用）正确更新了 `rimeScorer`，因此测试中问题不会暴露。

### 修复
`SetUnigram` 同步重建 `scorer` 和 `rimeScorer`。

## 问题 16：多字词评分使用字级估算而非词级频率

### 现象
输入 `rizhi` 首候选为"日至"而非常用词"日志"。两者权重仅差 ~10000（5077950 vs 5068055）。

### 根因
`RimeScorer.ScoreWithLM` 对多字词直接调用 `CharBasedScore`（单字频率平均值），绕过了 `LogProb`。`LogProb` 对多字词已内置 word-level → CharBasedScore 的 fallback 逻辑：如果词在 unigram 中存在则返回词级频率，否则 fallback 到字级平均。但 `ScoreWithLM` 未利用这一点。

"日志"在 unigram 中是常见词（词级 LogProb 高），"日至"不在（fallback 到字级平均），但两者 CharBasedScore 相近（"至"和"志"字频相近），导致无法区分。

### 修复
`ScoreWithLM` 统一使用 `LogProb` 替代条件分支，使 unigram 中的常见词组获得词级频率加成。
