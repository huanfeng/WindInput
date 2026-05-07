# WindInput 拼音数据源分析

## 1. 数据源概览

WindInput 拼音引擎使用以下核心数据源，均来自 [白霜拼音 (rime-frost)](https://github.com/gaboolic/rime-frost) 项目。
rime-frost 基于雾凇拼音 (rime-ice) 衍生，使用 745MB 语料重新计算词频，字词频率分布更贴近真实语料。

| 数据源 | 文件 | 规模 | 用途 |
|--------|------|------|------|
| 8105 | `cn_dicts/8105.dict.yaml` | ~8105 字 | 单字候选 + Unigram |
| 41448 | `cn_dicts/41448.dict.yaml` | ~41448 字 | 生僻字扩展候选 |
| base | `cn_dicts/base.dict.yaml` | ~36 万条 | 基础词组候选 + Unigram |
| ext | `cn_dicts/ext.dict.yaml` | ~23 万条 | 扩展词组候选 |
| others | `cn_dicts/others.dict.yaml` | 容错词 | 多音字异读容错 |
| corrections | `cn_dicts/corrections.dict.yaml` | 错音词 | weight=0，可查但不参与 Viterbi 路径 |

## 2. 数据使用方式

### 2.1 候选词库 (pinyin.wdb)

8105 + base 两个数据源编译生成 `pinyin.wdb`（约 30 MB 二进制文件），用于直接查询候选词。

构建流程：
```
8105.dict.yaml + base.dict.yaml → gen_bindict → pinyin.wdb (mmap 二进制格式)
```

二进制格式结构：
- 文件头 (32 bytes, 魔数 `WDIC`)
- KeyIndex 区：编码索引
- EntryRecords 区：候选词条（text + weight）
- StringPool 区：字符串存储池

### 2.2 Unigram 语言模型 (unigram.wdb)

三个数据源全部参与 Unigram 模型生成，合计约 152 万条词条。

构建流程：
```
8105 + base + tencent → gen_unigram → unigram.txt (152 万条) → unigram.wdb
```

优先级合并规则：8105 > base > tencent（同词取高优先级源的频次）。

### 2.3 关键限制

**tencent 98 万条仅用于语言模型，不能直接查询为候选。**

原因：
- tencent 数据源词条数量巨大（98 万），如果全部加入候选词库会导致候选列表过于庞杂
- 其中大量词条为低频词、专有名词，不适合作为通用候选
- 作为 Unigram 语言模型使用时，可以辅助 Viterbi 解码选择更自然的分词路径，而不会污染候选列表

## 3. 候选排序权重体系

WindInput 采用五层词库分层 + 多因子加权排序机制。

### 3.1 词库分层（优先级从高到低）

| 层级 | 类型 | 名称 | 说明 |
|------|------|------|------|
| Lv1 | Logic | 逻辑/指令层 | date, time, uuid 等命令候选 |
| Lv2 | Shadow | 用户修正层 | 置顶/删除/调权（权重 999999） |
| Lv3 | User | 用户造词层 | 用户自定义词条 |
| Lv4 | Cell | 细胞词库层 | 扩展词库（保留） |
| Lv5 | System | 系统主词库 | pinyin.wdb 核心词库 |

### 3.2 权重计算公式

```
score = freq * FreqWeight(1.0)
      + charCount * LengthBonus(10000)
      + sourceBonus (UserDict: 100000 | PhraseDict: 20000)
      + (IsExactMatch ? ExactMatchBonus(50000) : 0)
      + (UseLM ? lmScore * 10000 : 0)
```

### 3.3 候选排序规则

1. 权重降序（Weight 越大越靠前）
2. 同权重按文本升序（Unicode 顺序）
3. 再按编码升序
4. 最后按消耗长度降序（长词优先）

## 4. 与商业输入法的差距分析

### 4.1 已有能力

- Unigram 语言模型（152 万词条）
- Viterbi 解码（最优分词路径）
- Bigram 模型接口（已实现，可选启用）
- 用户词库自动学习
- Shadow 层修正机制
- 模糊音支持

### 4.2 主要差距

| 方面 | WindInput 现状 | 商业输入法 |
|------|---------------|-----------|
| N-gram | Unigram 为主，Bigram 可选 | Trigram + 神经网络语言模型 |
| Viterbi 调优 | 基础实现，权重配置有限 | 大量 A/B 测试调优 |
| 语料规模 | 25 亿字语料字频 | 千亿级语料 |
| 上下文理解 | 无 | 基于 Transformer 的上下文预测 |
| 纠错能力 | 模糊音映射 | 智能纠错 + 联想 |
| 个性化 | 基础用户词频 | 云端个性化模型 |

### 4.3 Viterbi 调优不足

当前 Viterbi 混合模型权重为固定值：
```
finalScore = 0.3 * unigramScore + 0.7 * bigramScore
```

未经过大规模语料验证调优，且缺少以下优化：
- 词频平滑处理（Good-Turing / Kneser-Ney）
- 未登录词概率估计
- 上下文相关的动态权重调整

## 5. 优化方向

### 5.1 短期（可快速实施）

- 优化 Ranker 权重参数（FreqWeight, LengthBonus 等），可基于用户反馈调整
- 完善用户词频学习：选词后自动提升权重，长期未用降权
- 丰富模糊音规则库

### 5.2 中期

- 引入高质量 Bigram 数据并启用 Viterbi Bigram 模式
- 基于用户输入历史训练个性化 Bigram
- 增加词频平滑算法
- 实现基于 Bigram 的智能纠错

### 5.3 长期

- 探索轻量级神经网络语言模型（如 N-gram LSTM）
- 云端个性化同步
- 上下文感知的候选排序

## 6. 用户词学习机制

### 6.1 自动造词流程

1. 用户输入拼音并选择候选词
2. 系统调用 `DictManager.AddUserWord(code, text, weight)` 自动记录
3. 初始权重设为 100
4. 词条写入用户词库文件（`pinyin_user_words.txt` 或 `wubi_user_words.txt`）
5. 异步保存机制：30 秒定期检查 + 事件驱动，合并多次修改以降低 I/O

### 6.2 存储格式

```
# WindInput 用户词库
# 格式: 编码<tab>词语<tab>权重<tab>时间戳
nihao	你好	150	2025-02-10T14:30:00Z
```

### 6.3 权重调整

- 通过 Shadow 层实现置顶（权重 999999）、删除（隐藏）、调权
- 用户词库初始权重 100，加上 UserDictBonus(100000) 在排序中获得较高优先级
- 运行时 Unigram 模型维护 `userFreqs` 频次追踪

### 6.4 已知问题

1. **缺少自动降权机制**：用户词一旦添加，权重不会因长期未使用而自然降低
2. **无选词频率追踪**：当前未记录候选被选中的次数，无法基于使用频率动态调整
3. **跨引擎不共享**：拼音和五笔用户词库独立存储，切换引擎后用户词不互通
4. **Shadow 与用户词库独立**：通过 Shadow 置顶的词条信息未反馈到用户词库的权重调整中
5. **Unigram userFreqs 不持久化**：运行时累积的用户选词频次在重启后丢失
