package pinyin

import (
	"strconv"
	"strings"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
)

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
func BuildLattice(input string, st *SyllableTrie, d dict.Dict, unigram *UnigramModel) *Lattice {
	n := len(input)
	lattice := &Lattice{
		nodes: make([][]LatticeNode, n+1),
		input: input,
	}

	// 获取词库的 Trie（如果可用）
	var dictTrie *dict.Trie
	if pd, ok := d.(*dict.PinyinDict); ok {
		dictTrie = pd.GetTrie()
	}

	// 构建 DAG
	dag := BuildDAG(input, st)

	// 边收集边查找：递归遍历 DAG，直接查词库，避免无效段
	seen := make(map[string]bool)
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
				key := latticeKey(startPos, pos, cand.Text)
				if seen[key] {
					continue
				}
				seen[key] = true

				logProb := calcLogProb(cand, unigram)
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
			if len(results) == 0 && dictTrie != nil {
				results = dictTrie.SearchPrefix(syllable, 5)
			}

			for _, cand := range results {
				key := latticeKey(startPos, endPos, cand.Text)
				if seen[key] {
					continue
				}
				seen[key] = true

				logProb := calcLogProb(cand, unigram)

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
func calcLogProb(cand candidate.Candidate, unigram *UnigramModel) float64 {
	if unigram != nil {
		return unigram.LogProb(cand.Text)
	}
	// 没有语言模型时使用权重的归一化值
	return float64(cand.Weight) / 100000.0
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
func latticeKey(start, end int, word string) string {
	return strconv.Itoa(start) + ":" + strconv.Itoa(end) + ":" + word
}

// LatticeFromCandidates 从候选词列表直接构建简单 Lattice
func LatticeFromCandidates(input string, syllables []string, candidates []candidate.Candidate, unigram *UnigramModel) *Lattice {
	n := len(input)
	lattice := &Lattice{
		nodes: make([][]LatticeNode, n+1),
		input: input,
	}

	for _, cand := range candidates {
		logProb := calcLogProb(cand, unigram)

		node := LatticeNode{
			Start:     0,
			End:       n,
			Word:      cand.Text,
			Syllables: syllables,
			LogProb:   logProb,
		}
		lattice.nodes[n] = append(lattice.nodes[n], node)
		lattice.size++
	}

	return lattice
}
