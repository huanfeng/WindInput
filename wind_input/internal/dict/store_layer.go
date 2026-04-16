package dict

import (
	"log/slog"
	"sort"
	"strings"
	"time"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/store"
)

// MaxDynamicWeight 用户词库动态权重硬上限
const MaxDynamicWeight = 2000

// ─────────────────────────────────────────
// StoreUserLayer — implements MutableLayer
// ─────────────────────────────────────────

// StoreUserLayer 基于 bbolt Store 的用户词库层，实现 MutableLayer 接口。
type StoreUserLayer struct {
	store    *store.Store
	schemaID string
	name     string
}

// NewStoreUserLayer 创建 StoreUserLayer。
func NewStoreUserLayer(s *store.Store, schemaID string) *StoreUserLayer {
	return &StoreUserLayer{
		store:    s,
		schemaID: schemaID,
		name:     "user:" + schemaID,
	}
}

// Name 返回层名称。
func (l *StoreUserLayer) Name() string { return l.name }

// Type 返回层类型。
func (l *StoreUserLayer) Type() LayerType { return LayerTypeUser }

// Search 精确查询用户词。
func (l *StoreUserLayer) Search(code string, limit int) []candidate.Candidate {
	code = strings.ToLower(code)
	recs, err := l.store.GetUserWords(l.schemaID, code)
	if err != nil {
		slog.Debug("StoreUserLayer.Search error", "code", code, "error", err)
		return nil
	}
	return userRecordsToCandidates(recs, code, limit)
}

// SearchPrefix 前缀查询用户词。
func (l *StoreUserLayer) SearchPrefix(prefix string, limit int) []candidate.Candidate {
	prefix = strings.ToLower(prefix)
	recs, err := l.store.SearchUserWordsPrefix(l.schemaID, prefix, limit)
	if err != nil {
		slog.Debug("StoreUserLayer.SearchPrefix error", "prefix", prefix, "error", err)
		return nil
	}
	return userRecordsToCandidates(recs, "", limit)
}

// Add 添加词条。
func (l *StoreUserLayer) Add(code string, text string, weight int) error {
	return l.store.AddUserWord(l.schemaID, strings.ToLower(code), text, weight)
}

// Remove 删除词条。
func (l *StoreUserLayer) Remove(code string, text string) error {
	return l.store.RemoveUserWord(l.schemaID, strings.ToLower(code), text)
}

// Update 更新词条权重。
func (l *StoreUserLayer) Update(code string, text string, newWeight int) error {
	return l.store.UpdateUserWordWeight(l.schemaID, strings.ToLower(code), text, newWeight)
}

// Save 无需手动保存（bbolt 自动持久化）。
func (l *StoreUserLayer) Save() error { return nil }

// EntryCount 返回词条总数。
func (l *StoreUserLayer) EntryCount() int {
	count, err := l.store.UserWordCount(l.schemaID)
	if err != nil {
		return 0
	}
	return count
}

// IncreaseWeight 增加词条权重，不超过 MaxDynamicWeight。
func (l *StoreUserLayer) IncreaseWeight(code, text string, delta int) {
	code = strings.ToLower(code)
	recs, err := l.store.GetUserWords(l.schemaID, code)
	if err != nil {
		return
	}
	for _, rec := range recs {
		if rec.Text == text {
			newWeight := rec.Weight + delta
			if newWeight > MaxDynamicWeight {
				newWeight = MaxDynamicWeight
			}
			_ = l.store.UpdateUserWordWeight(l.schemaID, code, text, newWeight)
			return
		}
	}
}

// OnWordSelected 带误选保护的选词回调。
// 若词条已存在则调用 store.OnWordSelected；否则用 addWeight 新增。
func (l *StoreUserLayer) OnWordSelected(code, text string, addWeight, boostDelta, countThreshold int) {
	code = strings.ToLower(code)
	recs, err := l.store.GetUserWords(l.schemaID, code)
	if err == nil {
		for _, rec := range recs {
			if rec.Text == text {
				_ = l.store.OnWordSelected(l.schemaID, code, text, boostDelta, countThreshold)
				return
			}
		}
	}
	// 词条不存在，新增
	_ = l.store.AddUserWord(l.schemaID, code, text, addWeight)
}

// ─────────────────────────────────────────
// StoreTempLayer — implements DictLayer
// ─────────────────────────────────────────

// StoreTempLayer 基于 bbolt Store 的临时词库层，实现 DictLayer 接口。
type StoreTempLayer struct {
	store        *store.Store
	schemaID     string
	name         string
	maxEntries   int
	promoteCount int
}

// NewStoreTempLayer 创建 StoreTempLayer。
func NewStoreTempLayer(s *store.Store, schemaID string) *StoreTempLayer {
	return &StoreTempLayer{
		store:    s,
		schemaID: schemaID,
		name:     "temp:" + schemaID,
	}
}

// SetLimits 设置最大条目数和晋升所需次数。
func (l *StoreTempLayer) SetLimits(maxEntries, promoteCount int) {
	l.maxEntries = maxEntries
	l.promoteCount = promoteCount
}

// Name 返回层名称。
func (l *StoreTempLayer) Name() string { return l.name }

// Type 返回层类型。
func (l *StoreTempLayer) Type() LayerType { return LayerTypeTemp }

// Search 精确查询临时词。
func (l *StoreTempLayer) Search(code string, limit int) []candidate.Candidate {
	code = strings.ToLower(code)
	recs, err := l.store.GetTempWords(l.schemaID, code)
	if err != nil {
		slog.Debug("StoreTempLayer.Search error", "code", code, "error", err)
		return nil
	}
	return userRecordsToCandidates(recs, code, limit)
}

// SearchPrefix 前缀查询临时词。
func (l *StoreTempLayer) SearchPrefix(prefix string, limit int) []candidate.Candidate {
	prefix = strings.ToLower(prefix)
	recs, err := l.store.SearchTempWordsPrefix(l.schemaID, prefix, limit)
	if err != nil {
		slog.Debug("StoreTempLayer.SearchPrefix error", "prefix", prefix, "error", err)
		return nil
	}
	return userRecordsToCandidates(recs, "", limit)
}

// LearnWord 学习新词。返回 true 表示词条已达到晋升条件。
func (l *StoreTempLayer) LearnWord(code, text string, weightDelta int) bool {
	code = strings.ToLower(code)
	if err := l.store.LearnTempWord(l.schemaID, code, text, weightDelta); err != nil {
		slog.Debug("StoreTempLayer.LearnWord error", "error", err)
		return false
	}

	// 按需淘汰
	if l.maxEntries > 0 {
		_, _ = l.store.EvictTempWords(l.schemaID, l.maxEntries)
	}

	// 检查是否达到晋升条件
	if l.promoteCount > 0 {
		recs, err := l.store.GetTempWords(l.schemaID, code)
		if err == nil {
			for _, rec := range recs {
				if rec.Text == strings.ToLower(text) && rec.Count >= l.promoteCount {
					return true
				}
			}
		}
	}
	return false
}

// PromoteWord 将词条晋升到用户词库。
func (l *StoreTempLayer) PromoteWord(code, text string) bool {
	code = strings.ToLower(code)
	text = strings.ToLower(text)
	err := l.store.PromoteTempWord(l.schemaID, code, text)
	if err != nil {
		slog.Debug("StoreTempLayer.PromoteWord error", "error", err)
		return false
	}
	return true
}

// GetWordCount 返回临时词条总数。
func (l *StoreTempLayer) GetWordCount() int {
	count, err := l.store.TempWordCount(l.schemaID)
	if err != nil {
		return 0
	}
	return count
}

// Clear 清空临时词库，返回删除的条目数。
func (l *StoreTempLayer) Clear() int {
	count, err := l.store.ClearTempWords(l.schemaID)
	if err != nil {
		return 0
	}
	return count
}

// ─────────────────────────────────────────
// StoreShadowLayer — implements ShadowProvider
// ─────────────────────────────────────────

// StoreShadowLayer 基于 bbolt Store 的 Shadow 规则层，实现 ShadowProvider 接口。
type StoreShadowLayer struct {
	store    *store.Store
	schemaID string
	name     string
}

// NewStoreShadowLayer 创建 StoreShadowLayer。
func NewStoreShadowLayer(s *store.Store, schemaID string) *StoreShadowLayer {
	return &StoreShadowLayer{
		store:    s,
		schemaID: schemaID,
		name:     "shadow:" + schemaID,
	}
}

// Name 返回层名称。
func (l *StoreShadowLayer) Name() string { return l.name }

// GetShadowRules 返回指定编码的 Shadow 规则，转换为 dict.ShadowRules。
func (l *StoreShadowLayer) GetShadowRules(code string) *ShadowRules {
	code = strings.ToLower(code)
	rec, err := l.store.GetShadowRules(l.schemaID, code)
	if err != nil {
		slog.Debug("StoreShadowLayer.GetShadowRules error", "code", code, "error", err)
		return nil
	}
	if len(rec.Pinned) == 0 && len(rec.Deleted) == 0 {
		return nil
	}
	rules := &ShadowRules{
		Deleted: rec.Deleted,
	}
	for _, p := range rec.Pinned {
		rules.Pinned = append(rules.Pinned, PinnedWord{
			Word:     p.Word,
			Position: p.Position,
		})
	}
	return rules
}

// Pin 固定词在指定位置。
func (l *StoreShadowLayer) Pin(code, word string, position int) {
	_ = l.store.PinShadow(l.schemaID, strings.ToLower(code), word, position)
}

// Delete 隐藏指定词。
func (l *StoreShadowLayer) Delete(code, word string) {
	_ = l.store.DeleteShadow(l.schemaID, strings.ToLower(code), word)
}

// RemoveRule 从 Pinned 和 Deleted 中移除指定词的所有规则。
func (l *StoreShadowLayer) RemoveRule(code, word string) {
	_ = l.store.RemoveShadowRule(l.schemaID, strings.ToLower(code), word)
}

// GetRuleCount 返回有规则的编码总数。
func (l *StoreShadowLayer) GetRuleCount() int {
	count, err := l.store.ShadowRuleCount(l.schemaID)
	if err != nil {
		return 0
	}
	return count
}

// IsDirty 始终返回 false（bbolt 自动持久化）。
func (l *StoreShadowLayer) IsDirty() bool { return false }

// ─────────────────────────────────────────
// StoreFreqScorer — implements FreqScorer
// ─────────────────────────────────────────

// StoreFreqScorer 基于 bbolt Store 的词频评分器，实现 FreqScorer 接口。
type StoreFreqScorer struct {
	store    *store.Store
	schemaID string
}

// NewStoreFreqScorer 创建 StoreFreqScorer。
func NewStoreFreqScorer(s *store.Store, schemaID string) *StoreFreqScorer {
	return &StoreFreqScorer{
		store:    s,
		schemaID: schemaID,
	}
}

// FreqBoost 返回候选词的词频加成分数。
func (f *StoreFreqScorer) FreqBoost(code, text string) int {
	rec, err := f.store.GetFreq(f.schemaID, code, text)
	if err != nil {
		return 0
	}
	return store.CalcFreqBoost(rec, time.Now().Unix())
}

// ─────────────────────────────────────────
// 内部辅助函数
// ─────────────────────────────────────────

// userRecordsToCandidates 将 store.UserWordRecord 切片转换为已排序的 candidate.Candidate 切片。
// code 参数用于精确查询（非空时覆盖 rec.Code），前缀查询时传空串则使用 rec.Code。
func userRecordsToCandidates(recs []store.UserWordRecord, code string, limit int) []candidate.Candidate {
	if len(recs) == 0 {
		return nil
	}
	results := make([]candidate.Candidate, 0, len(recs))
	for _, rec := range recs {
		candCode := code
		if candCode == "" {
			candCode = rec.Code // 前缀查询时从记录中获取
		}
		c := candidate.Candidate{
			Text:     rec.Text,
			Code:     candCode,
			Weight:   rec.Weight,
			IsCommon: true, // 用户词不应被 smart 过滤
		}
		results = append(results, c)
	}
	sort.Slice(results, func(i, j int) bool {
		return candidate.Better(results[i], results[j])
	})
	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}
	return results
}
