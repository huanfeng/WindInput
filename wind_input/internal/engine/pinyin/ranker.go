package pinyin

import (
	"math"
	"sort"

	"github.com/huanfeng/wind_input/internal/candidate"
)

// ============================================================
// Ranker 候选排序器
// 负责对词库查询结果进行排序和评分
// ============================================================

// RankerConfig 排序器配置
type RankerConfig struct {
	// 词频权重
	FreqWeight float64

	// 词长加成（每个字符的加成）
	LengthBonus int

	// 完整匹配加成
	ExactMatchBonus int

	// 用户词库加成
	UserDictBonus int

	// 短语词库加成
	PhraseDictBonus int

	// 启用语言模型评分
	UseLM bool
}

// DefaultRankerConfig 默认排序器配置
func DefaultRankerConfig() *RankerConfig {
	return &RankerConfig{
		FreqWeight:      1.0,
		LengthBonus:     10000,
		ExactMatchBonus: 50000,
		UserDictBonus:   100000,
		PhraseDictBonus: 20000,
		UseLM:           false,
	}
}

// Ranker 候选排序器
type Ranker struct {
	config  *RankerConfig
	unigram UnigramLookup
	bigram  *BigramModel
}

// NewRanker 创建排序器
func NewRanker(config *RankerConfig) *Ranker {
	if config == nil {
		config = DefaultRankerConfig()
	}
	return &Ranker{
		config: config,
	}
}

// SetUnigram 设置 Unigram 语言模型
func (r *Ranker) SetUnigram(m UnigramLookup) {
	r.unigram = m
}

// SetBigram 设置 Bigram 语言模型
func (r *Ranker) SetBigram(m *BigramModel) {
	r.bigram = m
}

// RankInput 排序输入参数
type RankInput struct {
	Entries      []LexiconEntry
	ParseResult  *ParseResult
	IsExactMatch bool
	PreviousWord string // 上一个词（用于 bigram）
}

// Rank 对候选词进行排序
// 返回排序后的 candidate.Candidate 列表
func (r *Ranker) Rank(input *RankInput) []candidate.Candidate {
	if input == nil || len(input.Entries) == 0 {
		return nil
	}

	// 计算每个候选的得分
	scored := make([]scoredCandidate, 0, len(input.Entries))
	for _, entry := range input.Entries {
		score := r.computeScore(entry, input)
		scored = append(scored, scoredCandidate{
			entry: entry,
			score: score,
		})
	}

	// 按得分降序排序
	sort.Slice(scored, func(i, j int) bool {
		return scored[i].score > scored[j].score
	})

	// 转换为 candidate.Candidate
	candidates := make([]candidate.Candidate, 0, len(scored))
	for _, sc := range scored {
		cand := candidate.Candidate{
			Text:     sc.entry.Text,
			Code:     joinSyllables(sc.entry.Syllables),
			Weight:   int(sc.score),
			IsCommon: sc.entry.Freq > 10000, // 简化判断
		}
		candidates = append(candidates, cand)
	}

	return candidates
}

// RankEntriesToCandidates 简化版排序，直接从 LexiconEntry 转换
func (r *Ranker) RankEntriesToCandidates(entries []LexiconEntry, isExactMatch bool) []candidate.Candidate {
	return r.Rank(&RankInput{
		Entries:      entries,
		IsExactMatch: isExactMatch,
	})
}

// scoredCandidate 带分数的候选
type scoredCandidate struct {
	entry LexiconEntry
	score float64
}

// computeScore 计算候选得分
func (r *Ranker) computeScore(entry LexiconEntry, input *RankInput) float64 {
	score := float64(entry.Freq) * r.config.FreqWeight

	// 词长加成
	charCount := len([]rune(entry.Text))
	score += float64(charCount * r.config.LengthBonus)

	// 来源加成
	switch entry.Source {
	case SourceUser:
		score += float64(r.config.UserDictBonus)
	case SourcePhrase:
		score += float64(r.config.PhraseDictBonus)
	}

	// 完整匹配加成
	if input.IsExactMatch {
		score += float64(r.config.ExactMatchBonus)
	}

	// 语言模型评分
	if r.config.UseLM && r.unigram != nil {
		lmScore := r.computeLMScore(entry, input)
		score += lmScore * 10000 // 缩放 LM 分数
	}

	return score
}

// computeLMScore 计算语言模型得分
func (r *Ranker) computeLMScore(entry LexiconEntry, input *RankInput) float64 {
	if r.unigram == nil {
		return 0
	}

	// Unigram 得分
	unigramScore := r.unigram.LogProb(entry.Text)

	// Bigram 得分（如果有上文）
	if r.bigram != nil && input.PreviousWord != "" {
		bigramScore := r.bigram.LogProb(input.PreviousWord, entry.Text)
		// 混合 unigram 和 bigram
		return 0.3*unigramScore + 0.7*bigramScore
	}

	return unigramScore
}

// joinSyllables 将音节列表拼接为字符串
func joinSyllables(syllables []string) string {
	if len(syllables) == 0 {
		return ""
	}
	result := syllables[0]
	for i := 1; i < len(syllables); i++ {
		result += syllables[i]
	}
	return result
}

// ============================================================
// 候选去重与合并
// ============================================================

// DeduplicateCandidates 对候选词去重，保留权重最高的
func DeduplicateCandidates(candidates []candidate.Candidate) []candidate.Candidate {
	seen := make(map[string]int) // text -> index
	result := make([]candidate.Candidate, 0, len(candidates))

	for _, cand := range candidates {
		if idx, exists := seen[cand.Text]; exists {
			// 已存在，保留权重更高的
			if cand.Weight > result[idx].Weight {
				result[idx] = cand
			}
		} else {
			seen[cand.Text] = len(result)
			result = append(result, cand)
		}
	}

	return result
}

// ============================================================
// Scorer 统一候选评分器
// 使用特征向量计算归一化分数，替代硬编码权重层级
// ============================================================

// Scorer 统一候选评分器
type Scorer struct {
	unigram UnigramLookup
	bigram  *BigramModel
}

// NewScorer 创建统一评分器
func NewScorer(unigram UnigramLookup, bigram *BigramModel) *Scorer {
	return &Scorer{unigram: unigram, bigram: bigram}
}

// Score 根据特征向量计算候选分数
// 返回值越大越优先
func (s *Scorer) Score(f CandidateFeatures) float64 {
	score := 0.0

	// 1. 来源基础分（决定大类优先级）
	if f.IsCommand {
		score += 4000
	} else if f.IsViterbi {
		score += 3000
	} else {
		switch f.MatchType {
		case MatchExact:
			score += 2000
		case MatchPartial:
			score += 1000
		case MatchFuzzy:
			score += 800
		}
	}

	// 2. 音节对齐奖励
	if f.SyllableMatch {
		score += 500
	}

	// 3. 语言模型分数（归一化到 0~400 区间）
	// LMScore 通常在 [-20, 0] 范围，线性映射
	lmNorm := (f.LMScore + 20.0) * 20.0 // [-20,0] → [0,400]
	if lmNorm < 0 {
		lmNorm = 0
	}
	if lmNorm > 400 {
		lmNorm = 400
	}
	score += lmNorm

	// 4. Bigram 上下文奖励
	if f.BigramScore != 0 {
		biNorm := (f.BigramScore + 20.0) * 10.0
		if biNorm < 0 {
			biNorm = 0
		}
		if biNorm > 200 {
			biNorm = 200
		}
		score += biNorm
	}

	// 5. 用户词加成
	if f.IsUserWord {
		score += 300
	}

	// 6. 词长奖励（鼓励长词匹配）
	score += float64(f.CharCount) * 20.0

	// 7. 惩罚项
	if f.IsFuzzy {
		score -= 100 // 模糊命中惩罚
	}
	if f.IsPartial {
		score -= 150 // partial 匹配惩罚
	}
	if f.IsAbbrev {
		score -= 50 // 简拼轻微惩罚
	}
	if f.SegmentRank > 0 {
		score -= float64(f.SegmentRank) * 30.0 // 非主路径惩罚
	}

	// 8. 词频分数（微调：字典频率仅用于同层级内细粒度排序）
	score += f.FreqScore * 0.00001

	return score
}

// MergeCandidates 合并多个候选列表，去重并重新排序
func MergeCandidates(lists ...[]candidate.Candidate) []candidate.Candidate {
	// 计算总容量
	total := 0
	for _, list := range lists {
		total += len(list)
	}

	// 合并
	merged := make([]candidate.Candidate, 0, total)
	for _, list := range lists {
		merged = append(merged, list...)
	}

	// 去重
	deduped := DeduplicateCandidates(merged)

	// 重新按权重排序
	sort.Slice(deduped, func(i, j int) bool {
		return deduped[i].Weight > deduped[j].Weight
	})

	return deduped
}

// ============================================================
// RimeScorer Rime 风格连续评分器
// 参照 librime script_translator.cc 的评分模型
// ============================================================

// rimeMaxDictWeight 词库最大权重（归一化基准）
const rimeMaxDictWeight = 1000000.0

// NormalizeWeight 将词库整数 weight 归一化到 [-15, 0] 区间
func NormalizeWeight(dictWeight float64) float64 {
	if dictWeight <= 0 {
		return -15.0
	}
	if dictWeight >= rimeMaxDictWeight {
		return 0.0
	}
	return (dictWeight/rimeMaxDictWeight)*15.0 - 15.0
}

// RimeScorer Rime 风格的连续评分器
type RimeScorer struct {
	unigram UnigramLookup
	bigram  *BigramModel
}

// NewRimeScorer 创建 Rime 风格评分器
func NewRimeScorer(unigram UnigramLookup, bigram *BigramModel) *RimeScorer {
	return &RimeScorer{unigram: unigram, bigram: bigram}
}

// Score 计算 Rime 风格的候选评分（不含 LM）
// normalizedWeight: 归一化后的词频权重 [-15, 0]
// initialQuality: 来源基础偏移
// coverage: 音节覆盖率 [0, 1]
func (s *RimeScorer) Score(normalizedWeight float64, initialQuality float64, coverage float64) float64 {
	return math.Exp(normalizedWeight) + initialQuality + coverage
}

// ScoreWithLM 带语言模型加成的 Rime 风格评分
// text: 候选文本（用于 LM 查询）
// dictWeight: 词库原始权重（整数，将被归一化）
// initialQuality: 来源基础偏移（见设计文档中的 initialQuality 值表）
// coverage: 音节覆盖率 [0, 1]（consumedSyllables / totalSyllables）
// charCount: 候选字符数（1=单字用 LogProb，>1 用 CharBasedScore）
func (s *RimeScorer) ScoreWithLM(text string, dictWeight float64, initialQuality float64, coverage float64, charCount int) float64 {
	nw := NormalizeWeight(dictWeight)
	// LM 加成
	if s.unigram != nil && text != "" {
		var lmScore float64
		if charCount == 1 {
			lmScore = s.unigram.LogProb(text)
		} else {
			lmScore = s.unigram.CharBasedScore(text)
		}
		nw += lmScore * 0.3
	}
	// 限制范围，防止极端值
	if nw > 0 {
		nw = 0
	}
	if nw < -20 {
		nw = -20
	}
	return math.Exp(nw) + initialQuality + coverage
}
