# 阶段三：评分体系重构 — 实施规划

## 一、目标

将拼音引擎的评分体系从"魔术数字硬墙分层"改为参照 Rime 的连续评分模型，
消除 SyllableMatch/FreqScore 清零等 hack，实现自然的候选排序。

## 二、Rime 原始公式

```cpp
// librime script_translator.cc
quality = exp(entry->weight) + initial_quality + (quality_len / full_code_length)
```

- `exp(entry->weight)`：词频的指数映射，weight 通常在 [-15, 0] 区间
- `initial_quality`：来源基础偏移（万象拼音：主翻译器=3~4，自定义短语=99）
- `quality_len / full_code_length`：覆盖率，0~1 的比例

## 三、适配设计

### 3.1 权重归一化

rime-ice 词库的 weight 是非负整数（如 800、1000、50000）。
需要映射到 Rime 期望的 [-15, 0] 区间：

```go
// 归一化公式：线性映射到 [-15, 0]
// maxDictWeight 取词库中的最大权重（预计算或使用固定值）
const maxDictWeight = 1000000.0 // rime-ice 词库的典型最大权重
normalizedWeight = (float64(dictWeight) / maxDictWeight) * 15.0 - 15.0
// dictWeight=0       → -15.0（极罕见）
// dictWeight=500000  → -7.5（中等）
// dictWeight=1000000 → 0.0（极常见）
```

### 3.2 initialQuality 设定

参照万象拼音的分层，适配我们的步骤结构：

| 步骤 | 来源 | initialQuality | 说明 |
|------|------|---------------|------|
| 步骤 0 | Command | 100.0 | 特殊命令最高（对应万象的99） |
| 步骤 1 | 精确匹配（全音节） | 4.0 | 对应万象主翻译器 |
| 步骤 1b | 多切分备选 | 3.5 | 略低于主路径 |
| 步骤 2 | 子词组（start=0） | 3.0 | 从首位开始的子序列 |
| 步骤 2 | 子词组（start>0） | 1.0 | 非首位，会丢失前导音节 |
| 步骤 3 | 前缀匹配 | 2.0 | 不完整匹配 |
| 步骤 4 | 首音节单字 | 2.5 | 用户最可能想选的首字 |
| 步骤 4 | 非首音节单字 | 0.5 | 消耗大量输入产出1字 |
| 步骤 4b | 多partial首字 | 2.0 | 纯partial输入 |
| 步骤 5 | partial前缀词组 | 1.5 | 不完整的前缀匹配 |
| 步骤 5 | partial展开单字 | 0.0 | 最低优先级 |
| 步骤 6 | 简拼（纯） | 3.0 | 纯首字母输入 |
| 步骤 6 | 简拼（有完整音节） | 1.0 | 简拼精确度低于子词组 |

### 3.3 覆盖率

```go
coverage = float64(consumedSyllableCount) / float64(totalSyllableCount)
// consumedSyllableCount: 该候选消耗的音节数（基于 ConsumedLength 反算）
// totalSyllableCount: parsed.Syllables 的总数
```

注：partial 展开的单字 consumedSyllableCount=0（它不真正"消耗"有效音节），
coverage=0，自然排到最低。

### 3.4 LM 加成（可选）

```go
if unigram != nil {
    lmScore = unigram.LogProb(text) // 通常 [-20, 0]
    // 直接加到 normalizedWeight 上
    normalizedWeight += lmScore * 0.3 // 缩放因子，避免 LM 权重过大
}
```

### 3.5 最终公式

```go
func (s *Scorer) ScoreRime(normalizedWeight float64, initialQuality float64, coverage float64) float64 {
    return math.Exp(normalizedWeight) + initialQuality + coverage
}
```

输出范围：
- `exp(-15)` ≈ 0.000000 → `exp(0)` = 1.0
- initialQuality: 0 ~ 100
- coverage: 0 ~ 1
- 总范围约 0 ~ 102（Command 约 101，普通候选约 0.5 ~ 6）

### 3.6 映射到 int Weight

coordinator 和 Shadow 层使用 int Weight 排序。需要映射：

```go
func (e *Engine) scorerWeight(score float64) int {
    // 乘以 1,000,000 保留精度，映射到与五笔引擎一致的范围
    return int(score * 1000000)
}
```

输出范围：
- Command: ~101,000,000
- 精确匹配高频词: ~5,000,000
- 子词组: ~4,000,000
- 首音节单字: ~3,500,000
- partial 展开: ~1,000,000
- 五笔引擎范围: 0 ~ 5,000,000（自然交错）

## 四、需要修改的文件

### 4.1 ranker.go — 核心评分重写

- 重写 `Scorer.Score()` → `Scorer.ScoreRime()`
- 删除旧的 CandidateFeatures 中大部分字段
- 新增 `RimeFeatures` 结构体：normalizedWeight, initialQuality, coverage

### 4.2 engine_ex.go — 各步骤适配

- 删除所有 `buildFeatures()` 调用，替换为直接计算 Rime 三元组
- 删除所有 hack：SyllableMatch、FreqScore 清零、MatchType 层级
- 每个步骤设定 initialQuality 值
- 计算 coverage = consumedSyllables / totalSyllables

### 4.3 types.go — 简化特征结构

- 可能删除 MatchType 枚举（不再需要 Exact/Partial/Fuzzy 分类）
- 或保留用于日志，但不参与评分

### 4.4 engine_ex_lookup.go — 子词组适配

- lookupSubPhrasesEx 中 start=0 和 start>0 使用不同 initialQuality
- 删除 isPartial hack

### 4.5 pinyin.go — 清理

- 删除旧的权重层级常量（weightCommand/weightExactMatch 等）
- 新增归一化常量

## 五、不需要修改的文件

- coordinator 层：只看 int Weight，无需变更
- bridge/IPC：透传 Weight，无需变更
- Shadow 层：操作 int Weight，置顶/删除不变，Reweight 旧数据可忽略
- 五笔引擎：完全独立，不受影响
- 用户词典：通过 LM boost 自然参与评分
- C++ TSF 层：不涉及

## 六、测试策略

1. 更新 realdict_test.go 中的 KeyAssertions（权重值会变，排序关系应保持）
2. 更新小词库测试的权重期望值
3. 新增：Rime 公式的单元测试（归一化、coverage 计算）
4. 保留：逐字符输入测试（验证排序关系不变）
5. 保留：ConsumedLength 边界测试（不涉及评分）

## 七、实施顺序

```
1. 新增 RimeScorer（新文件或 ranker.go 内），实现 Rime 评分公式
2. 新增归一化函数，适配 rime-ice 词库 weight
3. 逐步骤改造 engine_ex.go（每步骤替换一个，编译测试）
4. 改造 engine_ex_lookup.go（子词组）
5. 删除旧代码（buildFeatures、旧 Scorer、MatchType hack）
6. 更新所有测试期望值
7. 全量测试 + 真实词库验证
```
