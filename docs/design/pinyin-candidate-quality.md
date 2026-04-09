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
