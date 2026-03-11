<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# internal/engine/pinyin

## Purpose
拼音输入引擎实现。核心算法：

1. **音节解析**：`SyllableTrie` 匹配合法拼音音节，`Parser` 处理不完整输入
2. **DAG 构建**：`BuildDAG` 枚举所有可能的音节切分路径
3. **Viterbi 解码**：在词网格（`Lattice`）上用 Viterbi 算法找最优词序列
4. **候选排序**：`Ranker` 按权重/类型混排；`Composition` 管理当前拼音组合状态
5. **扩展功能**：模糊拼音（`FuzzyConfig`）、五笔反查提示、简拼索引、Unigram/Bigram 语言模型

## Key Files
| File | Description |
|------|-------------|
| `pinyin.go` | `Engine` 结构体、`Config`、构造函数、`Convert`/`ConvertRaw`、用户词频学习回调 |
| `engine_ex.go` | `ConvertEx` 入口：调用 convertCore，返回带组合态的扩展结果 |
| `engine_ex_lookup.go` | 候选查找逻辑：精确匹配、前缀匹配、简拼匹配、命令匹配 |
| `dag.go` | `DAG`、`DAGNode`、`BuildDAG`、`MaximumMatch`（正向最大匹配） |
| `viterbi.go` | `ViterbiDecode`：Viterbi 动态规划，支持 Unigram/Bigram 两种模式 |
| `lattice.go` | `Lattice`：词网格，存储每个位置的候选词节点及其概率 |
| `parser.go` | `Parser`：处理不完整音节输入（前缀匹配），分离已完成音节和未完成部分 |
| `syllable.go` | 所有合法普通话音节常量列表 |
| `syllable_trie.go` | `SyllableTrie`：音节前缀 Trie，`MatchAt` 返回从指定位置开始的所有匹配音节 |
| `fuzzy.go` | `FuzzyConfig`：模糊拼音规则（zh↔z、n↔l、an↔ang 等），展开变体 |
| `lm.go` | `UnigramModel`（内存）、`BinaryUnigramModel`（mmap）、`BigramModel` |
| `lexicon.go` | `Lexicon`：将词库查询结果填充到 Lattice |
| `composition.go` | `Composition`：管理当前输入的音节分段、光标位置、预编辑显示文本 |
| `ranker.go` | 候选排序：按配置的 `CandidateOrder` 混排（单字优先/词组优先/智能） |
| `types.go` | 拼音引擎内部类型定义 |

## For AI Agents

### Working In This Directory
- 智能组句（Viterbi）触发阈值：输入长度 ≥ 4（`smartComposeThreshold`）
- `FuzzyConfig` 为 nil 时不启用模糊拼音，启用后对每个音节展开变体加入搜索
- 五笔反查索引（`wubiReverse`）为懒加载，首次查询时从 `wubiTable` 构建
- `BinaryUnigramModel` 用 mmap 读取 unigram.wdb，几乎不占堆内存
- 用户词频学习：选词时对 `UserDict` 增加权重，同时 `BoostUserFreq` 更新 Unigram
- `DebugLog` 变量控制高频调试日志输出，生产环境保持 false

### Testing Requirements
- `go test ./internal/engine/pinyin/`
- 测试文件：`dag_test.go`、`viterbi_test.go`、`lattice_test.go`、`lexicon_test.go`、`lm_test.go`、`parser_test.go`、`syllable_trie_test.go`、`fuzzy_test.go`、`composition_test.go`、`engine_ex_test.go`、`pinyin_test.go`
- 算法测试不依赖词库文件，可独立运行

### Common Patterns
- `ConvertEx` 返回 `*ExResult`，包含 `Candidates`、`Composition`、`PreeditDisplay`、`IsEmpty`
- `Composition.PreeditDisplay` 格式：`"zhong'guo"` （音节间加分隔符）
- 模糊拼音变体展开在 `fuzzy.go` 的 `ExpandVariants` 函数中

## Dependencies
### Internal
- `internal/candidate` — Candidate 类型
- `internal/dict` — Dict 接口、DictManager、CodeTable
- `internal/dict/binformat` — BinaryUnigramModel mmap 读取

### External
- `math`（标准库）— 对数概率计算

<!-- MANUAL: -->
