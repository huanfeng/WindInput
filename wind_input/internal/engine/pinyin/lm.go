package pinyin

import (
	"bufio"
	"fmt"
	"math"
	"os"
	"strconv"
	"strings"
)

// UnigramModel 一元语言模型
type UnigramModel struct {
	logProbs  map[string]float64 // word -> log(P(word))
	total     float64            // 总频次
	minProb   float64            // 最小概率（用于未知词）
	userFreqs map[string]int     // 用户选词频次（运行时累积）
}

// NewUnigramModel 创建空的 Unigram 模型
func NewUnigramModel() *UnigramModel {
	return &UnigramModel{
		logProbs: make(map[string]float64),
	}
}

// Load 加载 Unigram 模型文件
// 格式: 词语\t频次
func (m *UnigramModel) Load(path string) error {
	file, err := os.Open(path)
	if err != nil {
		return fmt.Errorf("打开语言模型文件失败: %w", err)
	}
	defer file.Close()

	freqs := make(map[string]float64)
	var total float64

	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}

		parts := strings.Split(line, "\t")
		if len(parts) < 2 {
			continue
		}

		word := parts[0]
		freq, err := strconv.ParseFloat(parts[1], 64)
		if err != nil {
			continue
		}

		freqs[word] = freq
		total += freq
	}

	if err := scanner.Err(); err != nil {
		return fmt.Errorf("读取语言模型文件失败: %w", err)
	}

	if total == 0 {
		return fmt.Errorf("语言模型为空")
	}

	m.total = total
	m.minProb = math.Log(0.5 / total) // 未登录词概率

	// 计算 log 概率
	for word, freq := range freqs {
		m.logProbs[word] = math.Log(freq / total)
	}

	return nil
}

// LoadFromFreqMap 从频次映射构建模型（用于从词库直接生成）
func (m *UnigramModel) LoadFromFreqMap(freqs map[string]float64) {
	var total float64
	for _, freq := range freqs {
		total += freq
	}

	if total == 0 {
		return
	}

	m.total = total
	m.minProb = math.Log(0.5 / total)

	for word, freq := range freqs {
		m.logProbs[word] = math.Log(freq / total)
	}
}

// LogProb 获取词语的对数概率
// 如果用户有选词历史，会给予额外的概率提升
func (m *UnigramModel) LogProb(word string) float64 {
	baseProb := m.minProb
	if prob, ok := m.logProbs[word]; ok {
		baseProb = prob
	}

	// 用户频率提升：每次选词增加约 0.5 的 logprob 提升
	if m.userFreqs != nil {
		if freq, ok := m.userFreqs[word]; ok && freq > 0 {
			boost := float64(freq) * 0.5
			if boost > 5.0 {
				boost = 5.0 // 封顶，避免单词过度主导
			}
			return baseProb + boost
		}
	}

	return baseProb
}

// Contains 检查词语是否在模型中
func (m *UnigramModel) Contains(word string) bool {
	_, ok := m.logProbs[word]
	return ok
}

// Size 返回词汇量
func (m *UnigramModel) Size() int {
	return len(m.logProbs)
}

// BoostUserFreq 增加用户选词频次
func (m *UnigramModel) BoostUserFreq(word string, delta int) {
	if m.userFreqs == nil {
		m.userFreqs = make(map[string]int)
	}
	m.userFreqs[word] += delta
}

// LoadUserFreqs 从文件加载用户选词频次
// 格式: 词语\t频次
func (m *UnigramModel) LoadUserFreqs(path string) error {
	file, err := os.Open(path)
	if err != nil {
		if os.IsNotExist(err) {
			return nil // 文件不存在是正常的
		}
		return err
	}
	defer file.Close()

	m.userFreqs = make(map[string]int)
	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		parts := strings.Split(line, "\t")
		if len(parts) < 2 {
			continue
		}
		freq, err := strconv.Atoi(parts[1])
		if err != nil {
			continue
		}
		m.userFreqs[parts[0]] = freq
	}
	return scanner.Err()
}

// SaveUserFreqs 保存用户选词频次到文件
func (m *UnigramModel) SaveUserFreqs(path string) error {
	if m.userFreqs == nil || len(m.userFreqs) == 0 {
		return nil
	}

	file, err := os.Create(path)
	if err != nil {
		return err
	}
	defer file.Close()

	writer := bufio.NewWriter(file)
	writer.WriteString("# Wind Input 用户词频\n")
	for word, freq := range m.userFreqs {
		fmt.Fprintf(writer, "%s\t%d\n", word, freq)
	}
	return writer.Flush()
}

// GetUserFreqs 获取用户词频（用于持久化）
func (m *UnigramModel) GetUserFreqs() map[string]int {
	return m.userFreqs
}

// CharBasedScore 基于单字频率估算词组的常见程度
// 原理：常见词由常见字组成（如"这是"），而生僻词含生僻字（如"赭石"）
// 返回值越大表示越常见，取值范围为负数（log概率）
func (m *UnigramModel) CharBasedScore(word string) float64 {
	runes := []rune(word)
	if len(runes) == 0 {
		return m.minProb
	}

	var sum float64
	for _, r := range runes {
		sum += m.LogProb(string(r))
	}

	return sum / float64(len(runes))
}

// BigramModel 二元语言模型
type BigramModel struct {
	logProbs map[string]map[string]float64 // word1 -> word2 -> log(P(word2|word1))
	unigram  *UnigramModel                 // 回退模型
	lambda   float64                       // 插值系数
}

// NewBigramModel 创建 Bigram 模型
func NewBigramModel(unigram *UnigramModel) *BigramModel {
	return &BigramModel{
		logProbs: make(map[string]map[string]float64),
		unigram:  unigram,
		lambda:   0.7, // Bigram 权重
	}
}

// Load 加载 Bigram 模型文件
// 格式: 词1\t词2\t频次
func (m *BigramModel) Load(path string) error {
	file, err := os.Open(path)
	if err != nil {
		return fmt.Errorf("打开 Bigram 模型文件失败: %w", err)
	}
	defer file.Close()

	type pair struct {
		w1, w2 string
		freq   float64
	}
	var pairs []pair
	w1Totals := make(map[string]float64)

	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}

		parts := strings.Split(line, "\t")
		if len(parts) < 3 {
			continue
		}

		freq, err := strconv.ParseFloat(parts[2], 64)
		if err != nil {
			continue
		}

		pairs = append(pairs, pair{parts[0], parts[1], freq})
		w1Totals[parts[0]] += freq
	}

	if err := scanner.Err(); err != nil {
		return fmt.Errorf("读取 Bigram 模型失败: %w", err)
	}

	for _, p := range pairs {
		if _, ok := m.logProbs[p.w1]; !ok {
			m.logProbs[p.w1] = make(map[string]float64)
		}
		m.logProbs[p.w1][p.w2] = math.Log(p.freq / w1Totals[p.w1])
	}

	return nil
}

// LogProb 获取二元条件概率 P(word2|word1) 的对数
// 使用线性插值: λ * P_bigram + (1-λ) * P_unigram
func (m *BigramModel) LogProb(word1, word2 string) float64 {
	uniProb := m.unigram.LogProb(word2)

	if w2Map, ok := m.logProbs[word1]; ok {
		if biProb, ok := w2Map[word2]; ok {
			// 插值: log(λ * exp(biProb) + (1-λ) * exp(uniProb))
			return logSumExp(
				math.Log(m.lambda)+biProb,
				math.Log(1-m.lambda)+uniProb,
			)
		}
	}

	// 回退到 Unigram
	return uniProb
}

// logSumExp 计算 log(exp(a) + exp(b))，避免数值溢出
func logSumExp(a, b float64) float64 {
	if a > b {
		return a + math.Log1p(math.Exp(b-a))
	}
	return b + math.Log1p(math.Exp(a-b))
}
