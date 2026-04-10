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

// MaximumMatch 全局最优切分，返回覆盖最多输入字符的音节序列
// 使用动态规划而非贪心，避免局部最长匹配导致后续位置无法继续。
// 例如 "henihejiele"：贪心选 "hen" 后 "i" 无法匹配导致丢失 "ni"，
// DP 会选择 "he"+"ni"+"he"+"jie"+"le" 覆盖全部输入。
func (d *DAG) MaximumMatch() []string {
	n := len(d.input)
	if n == 0 {
		return nil
	}

	// dp[i] = 从位置 0 到位置 i 的最大覆盖字符数
	// prev[i] = 到达位置 i 时选择的音节（用于回溯）
	dp := make([]int, n+1)
	prev := make([]string, n+1)
	prevPos := make([]int, n+1)

	for i := range dp {
		dp[i] = -1 // 不可达
		prevPos[i] = -1
	}
	dp[0] = 0

	for pos := 0; pos < n; pos++ {
		if dp[pos] < 0 {
			continue // 此位置不可达
		}
		if pos >= len(d.nodes) {
			continue
		}
		for _, node := range d.nodes[pos] {
			end := node.End
			covered := dp[pos] + (end - pos)
			if covered > dp[end] {
				dp[end] = covered
				prev[end] = node.Syllables[0]
				prevPos[end] = pos
			}
		}
	}

	// 找到最远可达位置（优先覆盖全部输入，否则取最远）
	bestEnd := 0
	for i := n; i >= 0; i-- {
		if dp[i] >= 0 {
			bestEnd = i
			break
		}
	}
	if bestEnd == 0 {
		return nil
	}

	// 回溯构建音节序列
	var result []string
	for pos := bestEnd; pos > 0 && prevPos[pos] >= 0; pos = prevPos[pos] {
		result = append(result, prev[pos])
	}
	// 反转
	for i, j := 0, len(result)-1; i < j; i, j = i+1, j-1 {
		result[i], result[j] = result[j], result[i]
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
