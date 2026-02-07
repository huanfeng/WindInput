package pinyin

// DAGNode DAG 中的一个节点，代表从 Start 到 End 的音节序列
type DAGNode struct {
	Start     int      // 在原始输入中的起始位置
	End       int      // 在原始输入中的结束位置（不含）
	Syllables []string // 此节点对应的音节列表
}

// DAG 有向无环图，表示输入字符串的所有可能音节切分
type DAG struct {
	nodes [][]DAGNode // nodes[i] = 从位置 i 开始的所有可能节点
	input string
}

// BuildDAG 根据输入和音节 Trie 构建 DAG
func BuildDAG(input string, st *SyllableTrie) *DAG {
	n := len(input)
	dag := &DAG{
		nodes: make([][]DAGNode, n),
		input: input,
	}

	for i := 0; i < n; i++ {
		matches := st.MatchAt(input, i)
		for _, syllable := range matches {
			end := i + len(syllable)
			dag.nodes[i] = append(dag.nodes[i], DAGNode{
				Start:     i,
				End:       end,
				Syllables: []string{syllable},
			})
		}
	}

	return dag
}

// MaximumMatch 正向最大匹配，返回贪心切分的音节序列
func (d *DAG) MaximumMatch() []string {
	var result []string
	pos := 0
	n := len(d.input)

	for pos < n {
		if pos >= len(d.nodes) || len(d.nodes[pos]) == 0 {
			// 当前位置无法匹配任何完整音节，停止匹配
			// 余部交由 Parser 用 MatchPrefixAt 迭代处理
			break
		}

		// 选择最长的匹配
		best := d.nodes[pos][0] // 已经从长到短排序
		result = append(result, best.Syllables[0])
		pos = best.End
	}

	return result
}

// AllPaths 枚举所有可能的切分路径
// maxPaths 限制最大返回路径数
func (d *DAG) AllPaths(maxPaths int) [][]string {
	var results [][]string
	d.dfs(0, nil, &results, maxPaths)
	return results
}

// dfs 深度优先搜索所有路径
func (d *DAG) dfs(pos int, current []string, results *[][]string, maxPaths int) {
	if maxPaths > 0 && len(*results) >= maxPaths {
		return
	}

	if pos >= len(d.input) {
		if len(current) > 0 {
			path := make([]string, len(current))
			copy(path, current)
			*results = append(*results, path)
		}
		return
	}

	if pos >= len(d.nodes) || len(d.nodes[pos]) == 0 {
		return
	}

	for _, node := range d.nodes[pos] {
		if maxPaths > 0 && len(*results) >= maxPaths {
			return
		}
		d.dfs(node.End, append(current, node.Syllables[0]), results, maxPaths)
	}
}

// IsFullMatch 检查 DAG 是否覆盖了整个输入
func (d *DAG) IsFullMatch() bool {
	paths := d.AllPaths(1)
	return len(paths) > 0
}

// GetInput 获取原始输入
func (d *DAG) GetInput() string {
	return d.input
}
