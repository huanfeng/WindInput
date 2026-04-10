package pinyin

import (
	"log/slog"
	"math"
	"strings"
)

// ViterbiResult Viterbi 解码结果
type ViterbiResult struct {
	Words   []string // 最优词序列
	LogProb float64  // 路径总对数概率
}

// String 返回组句结果的字符串表示
func (r *ViterbiResult) String() string {
	return strings.Join(r.Words, "")
}

// ViterbiDecode 使用 Viterbi 算法从词网格中找到最优路径
// bigram 为 nil 时退化为 Unigram-only 模式
func ViterbiDecode(lattice *Lattice, bigram *BigramModel) *ViterbiResult {
	if lattice == nil || lattice.IsEmpty() {
		return nil
	}

	n := len(lattice.input)

	// dp[i] = 到达位置 i 的最优路径信息
	type dpEntry struct {
		logProb float64 // 累计对数概率
		prevPos int     // 前一个位置
		word    string  // 到达此位置的词
	}

	dp := make([]dpEntry, n+1)
	for i := range dp {
		dp[i].logProb = math.Inf(-1) // 初始化为负无穷
		dp[i].prevPos = -1
	}
	dp[0].logProb = 0 // 起点概率为 0（log(1)=0）

	logger := slog.Default()
	traceEnabled := n >= 8 // 仅对较长输入启用详细追踪

	// 前向传播
	for endPos := 1; endPos <= n; endPos++ {
		nodes := lattice.GetNodesEndingAt(endPos)
		for _, node := range nodes {
			startPos := node.Start

			if dp[startPos].logProb == math.Inf(-1) {
				continue // 起始位置不可达
			}

			// 计算转移概率
			var transProb float64
			if bigram != nil && dp[startPos].word != "" {
				// 使用 Bigram 概率
				transProb = bigram.LogProb(dp[startPos].word, node.Word)
			} else {
				// 使用 Unigram 概率
				transProb = node.LogProb
			}

			totalProb := dp[startPos].logProb + transProb

			if totalProb > dp[endPos].logProb {
				if traceEnabled {
					logger.Debug("[VITERBI_TRACE] dp_update",
						"pos", endPos, "word", node.Word,
						"from", startPos, "prevWord", dp[startPos].word,
						"transProb", transProb, "nodeLogProb", node.LogProb,
						"totalProb", totalProb,
						"oldBest", dp[endPos].word, "oldProb", dp[endPos].logProb)
				}
				dp[endPos].logProb = totalProb
				dp[endPos].prevPos = startPos
				dp[endPos].word = node.Word
			}
		}
	}

	// 打印最终 dp 状态
	if traceEnabled {
		for i := 0; i <= n; i++ {
			if dp[i].logProb > math.Inf(-1) {
				logger.Debug("[VITERBI_TRACE] dp_final", "pos", i, "word", dp[i].word, "logProb", dp[i].logProb, "prevPos", dp[i].prevPos)
			}
		}
		// 打印 endPos=n 的所有候选节点
		endNodes := lattice.GetNodesEndingAt(n)
		for _, nd := range endNodes {
			logger.Debug("[VITERBI_TRACE] end_node", "word", nd.Word, "start", nd.Start, "end", nd.End, "logProb", nd.LogProb)
		}
	}

	// 检查是否有到达终点的路径
	if dp[n].logProb == math.Inf(-1) {
		if traceEnabled {
			logger.Debug("[VITERBI_TRACE] no_path", "input", lattice.input, "n", n)
		}
		return nil
	}

	// 回溯：从终点回溯得到最优词序列
	var words []string
	pos := n
	for pos > 0 {
		if dp[pos].word == "" {
			if traceEnabled {
				logger.Debug("[VITERBI_TRACE] backtrack_break", "pos", pos, "emptyWord", true)
			}
			break
		}
		words = append(words, dp[pos].word)
		pos = dp[pos].prevPos
		if pos < 0 {
			break
		}
	}

	// 反转词序列
	for i, j := 0, len(words)-1; i < j; i, j = i+1, j-1 {
		words[i], words[j] = words[j], words[i]
	}

	if traceEnabled {
		logger.Debug("[VITERBI_TRACE] result",
			"input", lattice.input, "words", strings.Join(words, "|"),
			"logProb", dp[n].logProb)
	}

	return &ViterbiResult{
		Words:   words,
		LogProb: dp[n].logProb,
	}
}

// ViterbiTopK 获取 Top-K 个最优路径
// 使用逐次排除法：每次找到最优路径后，排除路径中的一条边再重新解码
func ViterbiTopK(lattice *Lattice, bigram *BigramModel, k int) []*ViterbiResult {
	if k <= 0 {
		return nil
	}

	best := ViterbiDecode(lattice, bigram)
	if best == nil {
		return nil
	}

	results := []*ViterbiResult{best}
	if k == 1 {
		return results
	}

	// 使用 Yen's 简化版：排除最优路径中的每条边，取次优路径
	seen := make(map[string]bool)
	seen[best.String()] = true

	// 对最优路径中的每个词，尝试排除该词后重新解码
	for i, excludeWord := range best.Words {
		// 计算该词在输入中的位置
		startPos := 0
		for j := 0; j < i; j++ {
			// 需要找到词在 lattice 中对应的位置
			// 简化处理：通过累积计算
			for endP := startPos + 1; endP <= len(lattice.input); endP++ {
				nodes := lattice.GetNodesEndingAt(endP)
				for _, node := range nodes {
					if node.Start == startPos && node.Word == best.Words[j] {
						startPos = endP
						goto nextWord
					}
				}
			}
		nextWord:
		}

		// 在该位置排除这个词，重新解码
		alt := viterbiDecodeExclude(lattice, bigram, startPos, excludeWord)
		if alt != nil && !seen[alt.String()] {
			seen[alt.String()] = true
			results = append(results, alt)
			if len(results) >= k {
				break
			}
		}
	}

	// 按概率降序排序
	for i := 1; i < len(results); i++ {
		for j := i; j > 0 && results[j].LogProb > results[j-1].LogProb; j-- {
			results[j], results[j-1] = results[j-1], results[j]
		}
	}

	return results
}

// viterbiDecodeExclude 排除指定位置的指定词后重新解码
func viterbiDecodeExclude(lattice *Lattice, bigram *BigramModel, excludeStart int, excludeWord string) *ViterbiResult {
	if lattice == nil || lattice.IsEmpty() {
		return nil
	}

	n := len(lattice.input)

	type dpEntry struct {
		logProb float64
		prevPos int
		word    string
	}

	dp := make([]dpEntry, n+1)
	for i := range dp {
		dp[i].logProb = math.Inf(-1)
		dp[i].prevPos = -1
	}
	dp[0].logProb = 0

	for endPos := 1; endPos <= n; endPos++ {
		nodes := lattice.GetNodesEndingAt(endPos)
		for _, node := range nodes {
			// 排除指定的边
			if node.Start == excludeStart && node.Word == excludeWord {
				continue
			}

			startPos := node.Start
			if dp[startPos].logProb == math.Inf(-1) {
				continue
			}

			var transProb float64
			if bigram != nil && dp[startPos].word != "" {
				transProb = bigram.LogProb(dp[startPos].word, node.Word)
			} else {
				transProb = node.LogProb
			}

			totalProb := dp[startPos].logProb + transProb
			if totalProb > dp[endPos].logProb {
				dp[endPos].logProb = totalProb
				dp[endPos].prevPos = startPos
				dp[endPos].word = node.Word
			}
		}
	}

	if dp[n].logProb == math.Inf(-1) {
		return nil
	}

	var words []string
	pos := n
	for pos > 0 {
		if dp[pos].word == "" {
			break
		}
		words = append(words, dp[pos].word)
		pos = dp[pos].prevPos
		if pos < 0 {
			break
		}
	}

	for i, j := 0, len(words)-1; i < j; i, j = i+1, j-1 {
		words[i], words[j] = words[j], words[i]
	}

	return &ViterbiResult{
		Words:   words,
		LogProb: dp[n].logProb,
	}
}
