<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-01 -->

# internal/engine/pinyin

## Purpose
拼音输入引擎实现。核心算法：

1. **音节解析**：`SyllableTrie` 匹配合法拼音音节，`Parser` 处理不完整输入
2. **DAG 构建**：`BuildDAG` 枚举所有可能的音节切分路径
3. **Viterbi 解码**：在词网格（`Lattice`）上用 Viterbi 算法找最优词序列
4. **候选排序**：`Ranker` 基于连续评分模型（`CandidateFeatures` + `RankerConfig`）打分；`Composition` 管理当前拼音组合状态
5. **扩展功能**：模糊拼音（`FuzzyConfig`）、五笔反查提示、简拼索引、Unigram/Bigram 语言模型

## Key Files
| File | Description |
|------|-------------|
| `pinyin.go` | `Engine` 结构体、`Config`（含 `EnableUserFreq`、`FilterMode`）、构造函数、`Convert`/`ConvertRaw`、`OnCandidateSelected` 词频回调 |
| `engine_ex.go` | `ConvertEx` 入口：调用 convertCore，返回 `PinyinConvertResult`（含 Composition 状态） |
| `engine_ex_lookup.go` | 候选查找逻辑：精确匹配、前缀匹配、简拼匹配、命令匹配 |
| `dag.go` | `DAG`、`DAGNode`、`BuildDAG`、`MaximumMatch`（正向最大匹配） |
| `viterbi.go` | `ViterbiDecode`：Viterbi 动态规划，支持 Unigram/Bigram 两种模式 |
| `lattice.go` | `Lattice`：词网格，存储每个位置的候选词节点及其概率 |
| `parser.go` | `Parser`：处理不完整音节输入（前缀匹配），分离已完成音节和未完成部分 |
| `syllable.go` | 所有合法普通话音节常量列表 |
| `syllable_trie.go` | `SyllableTrie`：音节前缀 Trie，`MatchAt` 返回从指定位置开始的所有匹配音节 |
| `fuzzy.go` | `FuzzyConfig`：模糊拼音规则（zh↔z、n↔l、an↔ang 等），展开变体 |
| `lm.go` | `UnigramModel`（内存）、`BinaryUnigramModel`（mmap，支持 `LoadUserFreqs`/`SaveUserFreqs`）、`BigramModel` |
| `lexicon.go` | `Lexicon`：将词库查询结果填充到 Lattice，标注 `EntrySource`（系统/用户/短语） |
| `composition.go` | `CompositionState`：管理当前输入的音节分段、光标位置、预编辑显示文本 |
| `ranker.go` | `Ranker`：基于 `CandidateFeatures` 的连续评分模型；`RankerConfig` 控制各项加成权重；`RankInput`/`RankResult` 为输入输出类型 |
| `types.go` | 拼音引擎内部类型定义：`ParsedSyllable`、`ParseResult`、`LexiconEntry`、`EntrySource`、`MatchType`、`CandidateFeatures`、`PinyinConvertResult` |

## For AI Agents

### Working In This Directory
- **评分系统已重构为连续评分模型**：`Ranker` 通过 `CandidateFeatures` 计算综合分数，不再使用离散分层排序；`RankerConfig` 的各项 Bonus 字段控制权重
- `CandidateFeatures.MatchType` 描述结构匹配质量（Exact/Partial/Fuzzy），`IsFuzzy` 是来源标记（是否经模糊音展开），两者可叠加
- 智能组句（Viterbi）触发阈值：输入长度 ≥ 4（`smartComposeThreshold`）
- `FuzzyConfig` 为 nil 时不启用模糊拼音，启用后对每个音节展开变体加入搜索
- 五笔反查索引（`wubiReverse`）为懒加载，首次查询时从 `wubiTable` 构建；`ReleaseWubiHint()` 可释放反查数据
- `BinaryUnigramModel` 用 mmap 读取 unigram.wdb，`LoadUserFreqs`/`SaveUserFreqs` 管理用户词频增量
- `Config.EnableUserFreq=true` 时 `OnCandidateSelected` 才更新词频；由 Schema 的 `learning.mode` 控制
- `ConvertEx` 返回 `PinyinConvertResult`（替代旧的 `*ExResult`），含 `Composition *CompositionState`
- `DebugLog` 变量控制高频调试日志输出，生产环境保持 false

### Testing Requirements
- `go test ./internal/engine/pinyin/`
- 测试文件：`dag_test.go`、`viterbi_test.go`、`lattice_test.go`、`lexicon_test.go`、`lm_test.go`、`parser_test.go`、`syllable_trie_test.go`、`fuzzy_test.go`、`composition_test.go`、`engine_ex_test.go`、`pinyin_test.go`、`ranker_test.go`、`pinyin_ranking_test.go`、`realdict_test.go`、`incremental_input_test.go`、`incremental_nizhibuzhidao_test.go`
- 算法测试（DAG/Viterbi/Parser 等）不依赖词库文件，可独立运行
- `realdict_test.go` 需要实际词库文件（构建环境中应存在）

### Common Patterns
- `ConvertEx` 返回 `PinyinConvertResult`，含 `Candidates`、`Composition`、`PreeditDisplay`、`IsEmpty`
- `CompositionState.PreeditDisplay` 格式：`"zhong'guo"` （音节间加分隔符）
- 模糊拼音变体展开在 `fuzzy.go` 的 `ExpandVariants` 函数中

## Dependencies
### Internal
- `internal/candidate` — Candidate 类型
- `internal/dict` — CompositeDict（替代旧 Dict 接口）、DictManager、CodeTable
- `internal/dict/binformat` — BinaryUnigramModel mmap 读取

### External
- `math`（标准库）— 对数概率计算

<!-- MANUAL: -->
