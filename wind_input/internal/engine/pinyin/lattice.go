package pinyin

import (
	"log/slog"
	"math"
	"strconv"
	"strings"
	"sync"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
)

// seenKey 是 BuildLattice 内部去重用的复合键。
// 直接用 struct 作为 map key，避免每次调用 latticeKey 拼接 string。
type seenKey struct {
	start int32
	end   int32
	word  string
}

// seenSetPool 复用 BuildLattice 的去重 map，避免每次按键都 make 一个 ~128 桶的 map。
// 池中元素是 *map 以便归还时直接清空原 map（mapclear 编译优化）后放回。
var seenSetPool = sync.Pool{
	New: func() any {
		m := make(map[seenKey]struct{}, 256)
		return &m
	},
}

func acquireSeenSet() *map[seenKey]struct{} {
	return seenSetPool.Get().(*map[seenKey]struct{})
}

func releaseSeenSet(mp *map[seenKey]struct{}) {
	// Go 编译器会把 for k := range m { delete(m, k) } 优化为 runtime.mapclear，
	// 比重新 make 更快、更省 GC。
	for k := range *mp {
		delete(*mp, k)
	}
	seenSetPool.Put(mp)
}

// ============================================================
// 虚词白名单与词性启发式
// ============================================================

// functionWords 高频虚词/功能词白名单。
// 这些单字在造句中经常作为独立词出现（助词、代词、介词、连词、副词等），
// 施加完整的 singleCharPenalty 会不公平地惩罚包含它们的合理路径。
var functionWords = map[string]bool{
	// 助词
	"了": true, "的": true, "地": true, "得": true, "着": true, "过": true,
	// 代词
	"我": true, "你": true, "他": true, "她": true, "它": true, "们": true,
	"这": true, "那": true,
	// 介词/连词
	"和": true, "与": true, "在": true, "把": true, "被": true, "让": true,
	"从": true, "到": true, "对": true, "向": true, "跟": true,
	// 副词
	"不": true, "没": true, "也": true, "都": true, "就": true, "才": true,
	"还": true, "又": true, "再": true, "很": true, "太": true, "最": true,
	// 高频动词/判断词
	"是": true, "有": true, "会": true, "能": true, "要": true, "可": true,
	"去": true, "来": true, "做": true, "说": true, "看": true, "想": true,
}

// particleSuffixes V+助词模式的尾字。
// 多字词以这些字结尾时（如"接了"、"看的"），通常是动词+语法助词的组合，
// 而非独立语义词（如"和解"、"今天"），应在 Viterbi 中降权。
var particleSuffixes = map[rune]bool{
	'了': true, '的': true, '着': true, '过': true, '得': true, '地': true,
}

// LatticeNode 词网格节点
type LatticeNode struct {
	Start     int      // 在输入中的起始字节位置
	End       int      // 在输入中的结束字节位置（不含）
	Word      string   // 对应的词语
	Syllables []string // 对应的音节列表
	LogProb   float64  // Unigram 对数概率
}

// Lattice 词网格，用于 Viterbi 解码
type Lattice struct {
	nodes [][]LatticeNode // nodes[endPos] = 结束于此位置的所有节点
	input string
	size  int // 节点总数
}

// BuildLattice 构建词网格
// 对于输入的每个音节切分位置，查找词库中的所有匹配词语
func BuildLattice(input string, st *SyllableTrie, d *dict.CompositeDict, unigram UnigramLookup) *Lattice {
	n := len(input)
	lattice := &Lattice{
		nodes: make([][]LatticeNode, n+1),
		input: input,
	}

	logger := slog.Default()
	traceEnabled := n >= 8

	// 单字惩罚参数：
	// - 普通单字施加完整惩罚，确保多字词路径优于单字拼凑
	// - 虚词（了/的/和/我 等）给予正加成：unigram 模型中 P(虚词)×P(实词) 天然偏低，
	//   若不补偿，填鸭式(3字) 会因消除了"是"节点而碾压 天涯(2字)+是(1字)
	const singleCharPenalty = -3.0
	const functionWordPenalty = 2.0
	// 多字词加成参数：
	// - unigram 中的实体词（不以助词结尾）获得词频挂钩的加分，高频词和长词获得更多加成
	//   公式：baseContentWordBonus × sqrt(charCount) × freqFactor
	//   freqFactor 基于 unigram LogProb 映射到 [0, 1]：(LogProb - logProbMin) / logProbRange
	//   使用 LogProb 而非 cand.Weight，因为 Weight 在文本词库下是原始值（可达数十万），
	//   在 WDB 词库下才是归一化值 [0, 10000]，不一致；而 LogProb 始终来自同一个 unigram 模型。
	//   乘以 sqrt(charCount) 使 3 字词（如"目的地"）比 2 字词（如"弟弟"）获得适度加成，
	//   补偿 Viterbi 中多节点路径的累加劣势，同时避免线性 charCount 导致低频长词
	//   反超高频短词（如"填鸭式"3字反超"天涯"2字）。
	// - 以助词结尾的多字词（如"接了""看的"）为 V+助词语法组合，施加惩罚
	const baseContentWordBonus = 3.0
	const logProbMin = -15.0  // 低频词 LogProb 下界（极罕见词约 -15）
	const logProbRange = 12.0 // LogProb 跨度（[-15, -3] → 12）
	const verbParticlePenalty = -1.0

	// CompositeDict 始终支持前缀搜索
	ps := d
	hasPrefixSearch := true

	// 构建 DAG
	dag := BuildDAG(input, st)

	if traceEnabled {
		// 打印 DAG 结构
		for i := 0; i < n && i < len(dag.nodes); i++ {
			for _, dn := range dag.nodes[i] {
				logger.Debug("[LATTICE_TRACE] dag_node", "pos", i, "end", dn.End, "syllable", dn.Syllables[0])
			}
		}
	}

	// 边收集边查找：递归遍历 DAG，直接查词库，避免无效段。
	// seen map 从 sync.Pool 复用，避免每次 BuildLattice 调用都新建 ~128 桶 map。
	seenPtr := acquireSeenSet()
	defer releaseSeenSet(seenPtr)
	seen := *seenPtr
	maxWordLen := 6 // 中文词语最长约 6 音节（成语/固定短语）
	maxNodes := 2000

	var collectAndLookup func(pos int, startPos int, syllables []string)
	collectAndLookup = func(pos int, startPos int, syllables []string) {
		if lattice.size >= maxNodes || len(syllables) > maxWordLen {
			return
		}

		if len(syllables) > 0 {
			code := strings.Join(syllables, "")
			results := d.Lookup(code)
			for _, cand := range results {
				// weight=0 为容错/错音词（corrections.dict.yaml），不参与 Viterbi 路径评分，
				// 与 YAML trie 行为一致（loadRimeFile 过滤了 weight<=0）。
				// 这些词仍可通过精确查找路径（ConvertEx 步骤 2-4）返回给用户。
				if cand.Weight == 0 {
					continue
				}
				key := seenKey{start: int32(startPos), end: int32(pos), word: cand.Text}
				if _, ok := seen[key]; ok {
					continue
				}
				seen[key] = struct{}{}

				logProb := calcLogProb(cand, unigram)
				runes := []rune(cand.Text)
				charCount := len(runes)
				if charCount == 1 {
					// 方案二：虚词白名单 — 虚词施加轻微惩罚，其他单字施加完整惩罚
					if functionWords[cand.Text] {
						logProb += functionWordPenalty
					} else {
						logProb += singleCharPenalty
					}
				} else if charCount > 1 {
					// 方案三：实体词加分 + V+助词惩罚
					if particleSuffixes[runes[charCount-1]] {
						// "接了""看的" 等 V+助词组合：降权
						logProb += verbParticlePenalty
					} else if unigram != nil && unigram.Contains(cand.Text) {
						freqFactor := (logProb - logProbMin) / logProbRange
						if freqFactor < 0 {
							freqFactor = 0
						} else if freqFactor > 1 {
							freqFactor = 1
						}
						bonus := baseContentWordBonus * math.Sqrt(float64(charCount)) * freqFactor
						logProb += bonus
					}
				}
				if traceEnabled && len([]rune(cand.Text)) > 1 {
					logger.Debug("[LATTICE_TRACE] multichar_node",
						"word", cand.Text, "code", code,
						"start", startPos, "end", pos,
						"logProb", logProb, "dictWeight", cand.Weight,
						"inUnigram", unigram != nil && unigram.Contains(cand.Text))
				}
				node := LatticeNode{
					Start:     startPos,
					End:       pos,
					Word:      cand.Text,
					Syllables: copySyllables(syllables),
					LogProb:   logProb,
				}
				lattice.nodes[pos] = append(lattice.nodes[pos], node)
				lattice.size++
			}
		}

		if pos >= n || pos >= len(dag.nodes) {
			return
		}

		for _, dagNode := range dag.nodes[pos] {
			collectAndLookup(dagNode.End, startPos, append(syllables, dagNode.Syllables[0]))
		}
	}

	// 从每个位置开始收集并查词
	for startPos := 0; startPos < n; startPos++ {
		if startPos < len(dag.nodes) && len(dag.nodes[startPos]) > 0 {
			for _, dagNode := range dag.nodes[startPos] {
				collectAndLookup(dagNode.End, startPos, []string{dagNode.Syllables[0]})
			}
		}
	}

	// 为每个单音节添加单字节点（确保至少有通路）
	for startPos := 0; startPos < n; startPos++ {
		if startPos >= len(dag.nodes) {
			continue
		}
		for _, dagNode := range dag.nodes[startPos] {
			syllable := dagNode.Syllables[0]
			endPos := dagNode.End

			results := d.Lookup(syllable)
			if len(results) == 0 && hasPrefixSearch {
				results = ps.LookupPrefix(syllable, 5)
			}

			for _, cand := range results {
				if cand.Weight == 0 {
					continue
				}
				key := seenKey{start: int32(startPos), end: int32(endPos), word: cand.Text}
				if _, ok := seen[key]; ok {
					continue
				}
				seen[key] = struct{}{}

				logProb := calcLogProb(cand, unigram)
				// 单字回退节点惩罚：虚词轻罚，其他重罚
				if len([]rune(cand.Text)) == 1 {
					if functionWords[cand.Text] {
						logProb += functionWordPenalty
					} else {
						logProb += singleCharPenalty
					}
				}

				node := LatticeNode{
					Start:     startPos,
					End:       endPos,
					Word:      cand.Text,
					Syllables: []string{syllable},
					LogProb:   logProb,
				}
				lattice.nodes[endPos] = append(lattice.nodes[endPos], node)
				lattice.size++
			}
		}
	}

	return lattice
}

// calcLogProb 计算节点的对数概率
func calcLogProb(cand candidate.Candidate, unigram UnigramLookup) float64 {
	if unigram == nil {
		return float64(cand.Weight) / 100000.0
	}

	charCount := len([]rune(cand.Text))

	// 单字或 unigram 模型中存在的词：直接使用 LogProb
	if charCount <= 1 || unigram.Contains(cand.Text) {
		return unigram.LogProb(cand.Text)
	}

	// 多字词不在 unigram 模型中：使用字符平均 LogProb 估算，
	// 并施加惩罚以区分"估算概率"与"实测概率"。
	// 高频字组合（如"接了"="接"+"了"）的 CharBasedScore 可能虚高，
	// 不应与 unigram 中有真实频率的词（如"和解"）平级竞争。
	const charBasedPenalty = -2.0
	return unigram.CharBasedScore(cand.Text) + charBasedPenalty
}

// GetNodesEndingAt 获取结束于指定位置的所有节点
func (l *Lattice) GetNodesEndingAt(pos int) []LatticeNode {
	if pos < 0 || pos >= len(l.nodes) {
		return nil
	}
	return l.nodes[pos]
}

// Size 返回节点总数
func (l *Lattice) Size() int {
	return l.size
}

// GetInput 获取原始输入
func (l *Lattice) GetInput() string {
	return l.input
}

// IsEmpty 检查网格是否为空
func (l *Lattice) IsEmpty() bool {
	return l.size == 0
}

func copySyllables(syllables []string) []string {
	result := make([]string, len(syllables))
	copy(result, syllables)
	return result
}

// latticeKey 生成节点去重 key
// 使用固定 buffer 减少临时字符串分配
func latticeKey(start, end int, word string) string {
	var buf [24]byte
	b := strconv.AppendInt(buf[:0], int64(start), 10)
	b = append(b, ':')
	b = strconv.AppendInt(b, int64(end), 10)
	b = append(b, ':')
	return string(b) + word
}
