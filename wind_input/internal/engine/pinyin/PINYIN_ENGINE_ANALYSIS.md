# WindInput 拼音引擎 `convertCore` 全流程逻辑链条文档

## 一、全局概览

`convertCore` 是 WindInput 拼音引擎的核心候选生成流水线，位于 `engine_ex.go:75-546`。它接收用户的原始按键输入字符串，经过 **解析 -> 多路候选生成 -> 评分 -> 排序 -> 过滤** 流水线，输出一组带权重的候选词列表。

整个流水线分为 **10 个阶段**：

```
输入 → 预处理 → Parser解析 → 组合态构建 → [步骤0~3g] 多路候选生成 → 列表化 → 排序 → Shadow规则 → 过滤 → 截断+五笔提示 → 输出
```

---

## 二、预处理阶段

### 2.1 输入规范化

| 项目 | 说明 | 代码位置 |
|------|------|----------|
| 空输入检查 | `len(input)==0` 直接返回 `IsEmpty=true` | `engine_ex.go:80-83` |
| 小写化 | `strings.ToLower(input)` | `engine_ex.go:85` |
| 去分隔符 | `queryInput = strings.ReplaceAll(input, "'", "")` 得到纯拼音用于词库查询 | `engine_ex.go:89` |

**关键区分**：`input` 保留原始 `'` 分隔符（用于计算 `ConsumedLength`、`len(input)`），`queryInput` 去掉 `'`（用于词库 key 查询）。

---

## 三、Parser 解析阶段

### 3.1 入口

```
parser := NewPinyinParserWithTrie(e.syllableTrie)
parsed := parser.Parse(input)
```

### 3.2 Parse 主流程

**分支逻辑**：
- 含 `'`：走 `parseWithSeparator`，按 `'` 切分后对每段调用 `parseSegment`
- 否则：直接调用 `parseSegment(input, 0, result)`

### 3.3 parseSegment 核心逻辑

1. **DAG 构建 + 最大匹配**：`BuildDAG(segment, syllableTrie)` → `dag.MaximumMatch()`
   - `BuildDAG`：遍历每个位置，调用 `syllableTrie.MatchAt(input, i)` 获取所有完整音节匹配
   - `MaximumMatch`：正向贪心，每次取最长匹配音节
   - 产出音节标记为 `SyllableExact`

2. **尾部残余处理**：从 `coveredEnd` 开始迭代处理：
   - 调用 `syllableTrie.MatchPrefixAt` 获取最长前缀
   - `isComplete=true` → `SyllableExact`
   - `isComplete=false` → `SyllablePartial`
   - 无匹配 → 单字符作为 `SyllablePartial`

### 3.4 产出的关键变量定义

| 变量 | 定义 | 举例 (`nihaozh`) |
|------|------|------------------|
| `completedSyllables` | **仅** `SyllableExact` 类型 | `["ni","hao"]` |
| `syllableCount` | `len(completedSyllables)` | `2` |
| `partial` | **最后一个** `SyllablePartial` 的文本 | `"zh"` |
| `allSyllables` | **所有**音节文本（Exact + Partial） | `["ni","hao","zh"]` |

**关键边界**：`PartialSyllable()` 只看最后一个音节。中间的 Partial 在 `completedSyllables` 中**不出现**。

### 3.5 `firstCompletedIsLeading` 标志

```go
firstCompletedIsLeading := syllableCount > 0 && len(allSyllables) > 0 &&
    allSyllables[0] == completedSyllables[0]
```

**语义**：第一个 Exact 音节是否也是输入的第一个段。反例：`sdem` → false。

---

## 四、多路候选生成阶段（步骤 0 ~ 3g）

所有候选汇入 `candidatesMap map[string]*candidate.Candidate`，以 `Text` 为 key 去重。

### 步骤 0：特殊命令精确匹配

| 项目 | 说明 |
|------|------|
| **触发条件** | 无条件 |
| **查询方式** | `dict.LookupCommand(queryInput)` |
| **MatchType** | `MatchExact` |
| **ConsumedLength** | `len(input)` |
| **权重** | Scorer `+4000` 基础分 → ~**4,000,000+** |

### 步骤 3a：Viterbi 智能组句

| 项目 | 说明 |
|------|------|
| **触发条件** | `UseSmartCompose` + `unigram != nil` + `syllableCount >= 2` + `len(completedCode) >= 4` |
| **输入选择** | 有 partial → `completedCode`；无 partial → `queryInput` |
| **MatchType** | `MatchExact` |
| **ConsumedLength** | 有 partial → `len(completedCode)`；无 → `len(input)` |
| **权重** | Scorer `+3000` → ~**3,000,000~3,400,000** |

### 步骤 3b：精确匹配完整音节序列

| 项目 | 说明 |
|------|------|
| **触发条件** | `syllableCount > 0` |
| **输入选择** | 有 partial → `completedCode`；无 → `queryInput` |
| **MatchType** | 默认 `MatchExact`；`hasExplicitSep && charCount != syllableCount` → `MatchPartial`；`!firstCompletedIsLeading` → `MatchPartial` |
| **ConsumedLength** | 有 partial → `len(completedCode)`；无 → `len(input)` |
| **权重** | `MatchExact` → ~**2,000,000~2,900,000** |

### 步骤 3b-alt：多切分并行打分

| 项目 | 说明 |
|------|------|
| **触发条件** | 无 `'` 且 `syllableCount > 0` |
| **ConsumedLength** | **始终** `len(input)` |
| **已知问题** | 包含 partial 后缀长度，选中后 partial 被吞掉 |

### 步骤 3c：前缀匹配

| 项目 | 说明 |
|------|------|
| **触发条件** | `syllableCount > 0` |
| **查询方式** | `dict.LookupPrefix(queryInput, limit)` |
| **MatchType** | `MatchPartial` |
| **ConsumedLength** | `len(input)` |

### 步骤 3d：子词组查找

| 项目 | 说明 |
|------|------|
| **触发条件** | `syllableCount > 1` |
| **查询方式** | 枚举所有连续子序列（长度 n 到 2），对每个 `lookupWithFuzzy` |
| **MatchType** | 全部 `MatchPartial` |
| **ConsumedLength** | `start==0` → 对应音节字节长度；`start>0` → 全部音节总长 |

### 步骤 3e：单字候选（三个子步骤）

**3e-leading（首段 Partial 音节单字）**：
- 触发：`syllableCount > 0 && !firstCompletedIsLeading`
- MatchType：`MatchExact`（首段最高优先级）
- ConsumedLength：`len(leadingPartial)`

**首音节单字**：
- 触发：`syllableCount > 0`
- MatchType：单音节→`MatchExact`；多音节→`MatchPartial`
- ConsumedLength：`len(firstSyllable)` + 前置 partial

**非首音节单字**：
- MatchType：`MatchPartial`
- ConsumedLength：`sum(completedSyllables[0:i+1])`
- **已知问题：不含前置 partial 段长度**

### 步骤 3f：未完成音节前缀查找

| 项目 | 说明 |
|------|------|
| **触发条件** | `partial != ""` |
| **3f-前缀词组** | `LookupPrefix(queryInput)`，单字→`MatchPartial`，多字→`MatchFuzzy` |
| **3f-单字展开** | `GetPossibleSyllables(partial)` → `Lookup(syllable)` |
| **ConsumedLength** | **始终** `len(input)` |
| **已知严重问题** | 多音节输入时，partial 单字消耗全部输入导致前面音节全部丢失 |

### 步骤 3g：简拼/混合简拼匹配

| 项目 | 说明 |
|------|------|
| **触发条件** | `len(allSyllables) >= 2` |
| **MatchType** | 纯缩写(`syllableCount==0`)→`MatchExact`；有完整音节→`MatchPartial` |
| **ConsumedLength** | `len(input)` |

---

## 五、权重计算完整链路

### Scorer.Score 评分公式

| 项 | 分值 | 乘1000后范围 |
|----|------|-------------|
| Command | +4000 | 4,000,000 |
| Viterbi | +3000 | 3,000,000 |
| MatchExact | +2000 | 2,000,000 |
| MatchPartial | +1000 | 1,000,000 |
| MatchFuzzy | +800 | 800,000 |
| SyllableMatch | +500 | 500,000 |
| LM 归一化 [0,400] | 0~400 | 0~400,000 |
| UserWord | +300 | 300,000 |
| CharCount × 20 | 20~120 | 20,000~120,000 |
| IsFuzzy 惩罚 | -100 | -100,000 |
| IsPartial 惩罚 | -150 | -150,000 |
| IsAbbrev 惩罚 | -50 | -50,000 |
| SegmentRank | -30/rank | -30,000/rank |
| FreqScore × 0.00001 | 极小 | <100 |

### 权重层级典型范围

| 类型 | 最低 | 最高 |
|------|------|------|
| Command | 4,020,000 | 4,720,000 |
| Viterbi | 3,020,000 | 3,720,000 |
| Exact+Aligned | 2,520,000 | 3,220,000 |
| Exact+Unaligned | 2,020,000 | 2,720,000 |
| Partial | 1,020,000 | 1,920,000 |
| Fuzzy | 700,000 | 1,520,000 |

---

## 六、ConsumedLength 计算汇总

| 步骤 | 有 partial 时 | 无 partial 时 |
|------|--------------|--------------|
| 0-Command | `len(input)` | `len(input)` |
| 3a-Viterbi | `len(completedCode)` | `len(input)` |
| 3b-Exact | `len(completedCode)` | `len(input)` |
| 3b-alt | `len(input)` ⚠ | `len(input)` |
| 3c-前缀 | `len(input)` | `len(input)` |
| 3d-子词组(start=0) | 部分音节长度 | 部分音节长度 |
| 3d-子词组(start>0) | 全部音节长度 | 全部音节长度 |
| 3e-首音节 | `len(firstSyllable)` | `len(firstSyllable)` |
| 3e-非首音节 | 累加(不含前置partial) ⚠ | 累加 |
| 3f-partial | `len(input)` ⚠⚠ | N/A |
| 3g-简拼 | `len(input)` | `len(input)` |

⚠ = 潜在问题，⚠⚠ = 严重问题

---

## 七、已知问题与设计缺陷汇总

### 严重

1. **步骤 3f partial 单字 ConsumedLength = len(input)**：多音节输入时，末尾 partial 展开的单字消耗全部输入，导致前面已完成音节全部丢失。例如 `wobuzhid` → "的"(consumed=8) 选中后丢失 `wobuzhi`。

2. **步骤 3e 非首音节 ConsumedLength 不含前置 partial**：选中后残留错误的输入片段。

### 中等

3. **步骤 3b-alt ConsumedLength 始终 len(input)**：包含 partial 后缀长度。

4. **smart 和 char_first 排序逻辑完全相同**：`char_first` 名不副实。

5. **Ranker 类未被使用**：旧代码残留。

6. **FilterCandidates 依赖未设置的 IsCommon 字段**：过滤行为不可预测。

### 低

7. **步骤 3d 子词组重建 DAG 冗余**：Parser 已做过相同操作。

8. **FreqScore 影响极小**：词频差异完全依赖 LM 间接体现。

---

## 八、重构方向建议

1. **统一 ConsumedLength 计算**：每个候选的 ConsumedLength 应精确反映它实际匹配的音节范围，而非简单使用 `len(input)`。核心原则：`ConsumedLength = sum(matched_syllables_in_input_bytes)`。

2. **步骤 3f 拆分**：partial 单字候选的 ConsumedLength 应仅消耗 partial 部分+对应位置之前的音节，而非整个输入。或者在分步确认场景中，partial 单字不应作为"消耗全部输入"的候选呈现。

3. **权重层级隔离**：确保不同来源路径的权重范围不重叠，避免 LM 分数导致低优先级候选跨层级跳到高优先级之上。

4. **清理旧代码**：移除未使用的 Ranker 类，统一排序逻辑。
