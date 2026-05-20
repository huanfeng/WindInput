package dict

import (
	"fmt"
	"log/slog"
	"sort"
	"strings"
	"sync"
	"unsafe"

	"github.com/huanfeng/wind_input/internal/candidate"
)

// uintptrOf 返回指针的整数形式 (调试日志用), 便于区分多 CompositeDict 实例。
func uintptrOf(p any) uintptr {
	switch v := p.(type) {
	case *CompositeDict:
		return uintptr(unsafe.Pointer(v))
	default:
		return 0
	}
}

// layerHit LookupCommand 调试日志条目, 见 formatLayerTrace。
type layerHit struct {
	name      string
	supported bool
	count     int
}

// formatLayerTrace 把 LookupCommand 内部 layer 命中记录拍扁为单行字符串,
// 例: "user:pinyin=0|phrase=93|codetable-system=skip"
func formatLayerTrace(trace []layerHit) string {
	parts := make([]string, 0, len(trace))
	for _, t := range trace {
		if !t.supported {
			parts = append(parts, fmt.Sprintf("%s=skip", t.name))
		} else {
			parts = append(parts, fmt.Sprintf("%s=%d", t.name, t.count))
		}
	}
	return strings.Join(parts, "|")
}

// CompositeDict 聚合词库
// 按优先级组合多个词库层，实现分层叠加查询
type CompositeDict struct {
	mu     sync.RWMutex
	layers []DictLayer // 按优先级排序（LayerType 小的在前）

	// Shadow 规则提供者（可选）
	shadowProvider ShadowProvider

	// 词频评分器（可选）
	freqScorer FreqScorer

	// 排序模式
	sortMode candidate.CandidateSortMode
}

// defaultPrefixSafeLimit 根据前缀长度计算底层 layer 的安全候选上限。
// 短前缀候选池天然庞大，给更大的窗口避免按字母序遍历时把高权重候选挡在外面；
// 长前缀候选自然收敛，无需放大。
func defaultPrefixSafeLimit(prefixLen int) int {
	switch prefixLen {
	case 0, 1:
		return 200
	case 2:
		return 800
	case 3:
		return 500
	default:
		return 300
	}
}

// NewCompositeDict 创建聚合词库
func NewCompositeDict() *CompositeDict {
	return &CompositeDict{
		layers: make([]DictLayer, 0),
	}
}

// AddLayer 添加词库层
// 层会按 Type() 返回的优先级自动排序
func (c *CompositeDict) AddLayer(layer DictLayer) {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.layers = append(c.layers, layer)

	// 按优先级排序（LayerType 小的优先级高，排在前面）
	sort.Slice(c.layers, func(i, j int) bool {
		return c.layers[i].Type() < c.layers[j].Type()
	})
}

// RemoveLayer 移除指定名称的词库层
func (c *CompositeDict) RemoveLayer(name string) bool {
	c.mu.Lock()
	defer c.mu.Unlock()

	for i, layer := range c.layers {
		if layer.Name() == name {
			c.layers = append(c.layers[:i], c.layers[i+1:]...)
			return true
		}
	}
	return false
}

// SetShadowProvider 设置 Shadow 规则提供者
func (c *CompositeDict) SetShadowProvider(provider ShadowProvider) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.shadowProvider = provider
}

// SetFreqScorer 设置词频评分器
func (c *CompositeDict) SetFreqScorer(scorer FreqScorer) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.freqScorer = scorer
}

// SetSortMode 设置候选排序模式
func (c *CompositeDict) SetSortMode(mode candidate.CandidateSortMode) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.sortMode = mode
}

// GetSortMode 获取当前排序模式
func (c *CompositeDict) GetSortMode() candidate.CandidateSortMode {
	c.mu.RLock()
	defer c.mu.RUnlock()
	if c.sortMode == "" {
		return candidate.SortByFrequency
	}
	return c.sortMode
}

// Search 聚合查询
// 按优先级遍历所有层，合并结果，应用 Shadow 规则
func (c *CompositeDict) Search(code string, limit int) []candidate.Candidate {
	c.mu.RLock()
	defer c.mu.RUnlock()

	return c.searchInternal(code, limit, false)
}

// SearchPrefix 聚合前缀查询
func (c *CompositeDict) SearchPrefix(prefix string, limit int) []candidate.Candidate {
	c.mu.RLock()
	defer c.mu.RUnlock()

	return c.searchInternal(prefix, limit, true)
}

// searchInternal 内部查询逻辑
// Shadow 的 pin/delete 不在此处处理——统一由引擎层 Phase 6（ApplyShadowPins）在最终排序后应用。
// CompositeDict 只负责层级合并、去重和基础排序。
func (c *CompositeDict) searchInternal(code string, limit int, isPrefix bool) []candidate.Candidate {
	// 1. 遍历所有层收集候选词
	// 去重策略：保留高优先级层（先出现）的词条信息，但继承后续层中同 Text 词条的更高权重。
	// 这确保用户词不会因为低权重而丢失码表词的自然排序位置。
	seenIdx := make(map[string]int) // Text -> index in results
	var results []candidate.Candidate

	// 前缀查询对底层传递安全限制，避免短前缀（如单字母"s"）触发全量 Trie 遍历。
	// 精确匹配不受影响（候选数天然有限）。
	// 上限按前缀长度分级：越短的前缀允许越大的窗口，避免高权重候选被字母序截断
	// （例如 jidian 词库中 `swy` 段会被一律 cap 在 ~600 条以前）。
	prefixSafeLimit := defaultPrefixSafeLimit(len(code))
	for _, layer := range c.layers {
		var layerResults []candidate.Candidate
		if isPrefix {
			layerLimit := prefixSafeLimit
			if limit > 0 && limit > prefixSafeLimit {
				layerLimit = limit
			}
			layerResults = layer.SearchPrefix(code, layerLimit)
		} else {
			layerResults = layer.Search(code, 0)
		}

		for _, cand := range layerResults {
			if idx, exists := seenIdx[cand.Text]; exists {
				// 同 Text 词条已存在：继承更高的权重
				if cand.Weight > results[idx].Weight {
					results[idx].Weight = cand.Weight
				}
				// 前缀搜索：同 Text 有多个编码时，保留最短码的 Code 和 NaturalOrder。
				// 最短码离输入最近，其 NaturalOrder 代表该字词在词库中最早出现的位置。
				// 不按此修正会导致代表条目携带长码高权重的 NaturalOrder（偏后），
				// 使该候选在自然顺序排序中错排到后面。
				if isPrefix && len(cand.Code) < len(results[idx].Code) {
					results[idx].Code = cand.Code
					results[idx].NaturalOrder = cand.NaturalOrder
				}
				continue
			}
			seenIdx[cand.Text] = len(results)
			results = append(results, cand)
		}
	}

	// 2. 应用词频加成
	if c.freqScorer != nil {
		for i := range results {
			boost := c.freqScorer.FreqBoost(results[i].Code, results[i].Text)
			if boost > 0 {
				results[i].Weight += boost
			}
		}
	}

	// 3. 排序
	comparator := candidate.Better
	if c.sortMode == candidate.SortByNatural {
		comparator = candidate.BetterNatural
	}
	sort.SliceStable(results, func(i, j int) bool {
		return comparator(results[i], results[j])
	})

	// 4. 限制返回数量
	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}

	return results
}

// ApplyShadowPins 在已排序的候选列表上应用 Shadow 的 pin（位置固定）和 delete（隐藏）规则。
// 这是引擎 Phase 6 的统一拦截器，五笔和拼音共用。
//
// 匹配优先级 (2026-05-17 R2): rule.CandID 非空时按 cand.ID 精准匹配
// (动态短语场景, id 跨日子稳定); CandID 空时按 rule.Word 匹配 cand.Text
// (兼容手输文本规则)。详见 docs/design/command-bar-followup.md R2。
//
// 处理逻辑：
//  1. 移除 deleted 中的词（单字跳过）
//  2. 提取有 pin 规则的候选
//  3. 按 pin position 放置（LIFO 碰撞顺延）
//  4. 未被 pin 的候选按原始顺序填充剩余位置
func ApplyShadowPins(candidates []candidate.Candidate, rules *ShadowRules) []candidate.Candidate {
	if rules == nil || (len(rules.Pinned) == 0 && len(rules.Deleted) == 0) {
		return candidates
	}

	// candMatchesDel/Pin: 统一的双字段匹配逻辑
	candMatchesDel := func(c candidate.Candidate, d DeletedWord) bool {
		if d.CandID != "" {
			return c.ID == d.CandID
		}
		return c.Text == d.Word
	}
	candMatchesPin := func(c candidate.Candidate, p PinnedWord) bool {
		if p.CandID != "" {
			return c.ID == p.CandID
		}
		return c.Text == p.Word
	}

	// 1. 过滤 deleted (单字按 Word 跳过, 短语 / cand 带 id 时不受单字保护)
	isDeleted := func(c candidate.Candidate) bool {
		for _, d := range rules.Deleted {
			// 单字保护: 仅当 Word 是单字且无 CandID 时跳过 (旧"单字不删"语义)
			if d.CandID == "" && len([]rune(d.Word)) <= 1 {
				continue
			}
			if candMatchesDel(c, d) {
				return true
			}
		}
		return false
	}

	// 2. 拆分 unpinned / pinnedCands
	// pinnedCands 键: rules.Pinned 的索引 → 命中的候选
	var unpinned []candidate.Candidate
	pinnedCands := make(map[int]candidate.Candidate, len(rules.Pinned))
	for _, c := range candidates {
		if isDeleted(c) {
			continue
		}
		hit := -1
		for i, p := range rules.Pinned {
			if _, taken := pinnedCands[i]; taken {
				continue // 同一 pin 规则只匹配一个候选
			}
			if candMatchesPin(c, p) {
				hit = i
				break
			}
		}
		if hit >= 0 {
			pinnedCands[hit] = c
		} else {
			unpinned = append(unpinned, c)
		}
	}

	// 3. 按 pin 规则分配槽位 (LIFO: 数组前面优先级高)
	slots := make(map[int]candidate.Candidate)
	usedPositions := make(map[int]bool)

	for i, pin := range rules.Pinned {
		cand, exists := pinnedCands[i]
		if !exists {
			continue // pin 的目标不在候选列表中（词库 / 短语模板变更后自然失效）
		}

		pos := pin.Position
		if pos < 0 {
			pos = 0
		}

		// 碰撞顺延: 找到最近的空槽位
		for usedPositions[pos] {
			pos++
		}
		slots[pos] = cand
		usedPositions[pos] = true
	}

	// 4. 合并: pin 词插入指定位置, unpinned 填充剩余
	totalLen := len(slots) + len(unpinned)
	result := make([]candidate.Candidate, 0, totalLen)
	unpinnedIdx := 0

	for i := 0; i < totalLen; i++ {
		if cand, ok := slots[i]; ok {
			result = append(result, cand)
		} else if unpinnedIdx < len(unpinned) {
			result = append(result, unpinned[unpinnedIdx])
			unpinnedIdx++
		}
	}
	// 追加剩余 unpinned (pin position 超出范围时)
	for unpinnedIdx < len(unpinned) {
		result = append(result, unpinned[unpinnedIdx])
		unpinnedIdx++
	}

	return result
}

// HasLongerCodeProvider 由 layer 选择性实现的"是否存在更长后继 code"探针接口。
// 实现该接口的 layer 可直接返回判定结果，避免 fallback 路径调 LookupPrefix 拉候选。
type HasLongerCodeProvider interface {
	HasLongerCode(input string) bool
}

// HasLongerCode 跨所有 layer 探测：是否存在 code != input && strings.HasPrefix(code, input) 的条目。
// 任一 layer 命中即短路返回 true。
//
// 探测策略：
//  1. 若 layer 实现 HasLongerCodeProvider，直接调；
//  2. 否则若 layer 实现 LookupPrefix(prefix, limit)，调 LookupPrefix(input, 1) 扫返回的候选看 Code != input。
func (c *CompositeDict) HasLongerCode(input string) bool {
	if input == "" {
		return false
	}
	c.mu.RLock()
	defer c.mu.RUnlock()
	for _, layer := range c.layers {
		if prov, ok := layer.(HasLongerCodeProvider); ok {
			if prov.HasLongerCode(input) {
				return true
			}
			continue
		}
		if pl, ok := layer.(interface {
			LookupPrefix(prefix string, limit int) []candidate.Candidate
		}); ok {
			for _, cand := range pl.LookupPrefix(input, 4) {
				if cand.Code != input {
					return true
				}
			}
			continue
		}
		// 兜底：用 SearchPrefix 拉少量结果探测
		for _, cand := range layer.SearchPrefix(input, 4) {
			if cand.Code != input {
				return true
			}
		}
	}
	return false
}

// GetLayers 获取所有层（用于调试）
func (c *CompositeDict) GetLayers() []DictLayer {
	c.mu.RLock()
	defer c.mu.RUnlock()

	result := make([]DictLayer, len(c.layers))
	copy(result, c.layers)
	return result
}

// GetLayerByName 根据名称获取层
func (c *CompositeDict) GetLayerByName(name string) DictLayer {
	c.mu.RLock()
	defer c.mu.RUnlock()

	for _, layer := range c.layers {
		if layer.Name() == name {
			return layer
		}
	}
	return nil
}

// GetLayersByType 根据类型获取层
func (c *CompositeDict) GetLayersByType(layerType LayerType) []DictLayer {
	c.mu.RLock()
	defer c.mu.RUnlock()

	var result []DictLayer
	for _, layer := range c.layers {
		if layer.Type() == layerType {
			result = append(result, layer)
		}
	}
	return result
}

// ============================================================
// 查询便捷方法
// ============================================================

// Lookup 按编码查询候选词
func (c *CompositeDict) Lookup(pinyin string) []candidate.Candidate {
	results := c.Search(pinyin, 0)
	// log.Printf("[CompositeDict] Lookup: pinyin=%q results=%d", pinyin, len(results))
	return results
}

// LookupPhrase 将音节列表拼接后查询
func (c *CompositeDict) LookupPhrase(syllables []string) []candidate.Candidate {
	if len(syllables) == 0 {
		return nil
	}

	// 拼接音节
	code := ""
	for _, s := range syllables {
		code += s
	}

	return c.Search(code, 0)
}

// LookupPrefix 实现 dict.PrefixSearchable 接口
func (c *CompositeDict) LookupPrefix(prefix string, limit int) []candidate.Candidate {
	return c.SearchPrefix(prefix, limit)
}

// LookupCommand 实现 dict.CommandSearchable 接口
// 仅查找特殊命令（uuid, date 等），不返回普通词条
//
// 调试日志 (2026-05-18): 遍历过程中记录每层是否实现 SearchCommand 与其返回
// 数量, 用于排查 "PhraseLayer 突然查不到 zzbd nav" 类瞬时窗口问题
// (schema 切换 / 临时拼音状态机残留 / 多 CompositeDict 实例)。
func (c *CompositeDict) LookupCommand(code string) []candidate.Candidate {
	c.mu.RLock()
	defer c.mu.RUnlock()

	trace := make([]layerHit, 0, len(c.layers))
	defer func() {
		// 在锁释放前算 trace 字符串; DEBUG 级别, 无原文。
		slog.Debug("composite.LookupCommand trace",
			"code", code, "dictPtr", uintptrOf(c), "layerCount", len(c.layers),
			"trace", formatLayerTrace(trace))
	}()

	for _, layer := range c.layers {
		cl, supported := layer.(interface {
			SearchCommand(code string, limit int) []candidate.Candidate
		})
		if !supported {
			trace = append(trace, layerHit{name: layer.Name(), supported: false})
			continue
		}
		results := cl.SearchCommand(code, 0)
		trace = append(trace, layerHit{name: layer.Name(), supported: true, count: len(results)})
		if len(results) > 0 {
			return results
		}
	}
	return nil
}

// LookupAbbrev 实现 dict.AbbrevSearchable 接口
// 遍历所有层查找简拼匹配
func (c *CompositeDict) LookupAbbrev(code string, limit int) []candidate.Candidate {
	c.mu.RLock()
	defer c.mu.RUnlock()

	seen := make(map[string]bool)
	var results []candidate.Candidate

	for _, layer := range c.layers {
		if al, ok := layer.(interface {
			SearchAbbrev(code string, limit int) []candidate.Candidate
		}); ok {
			layerResults := al.SearchAbbrev(code, 0)
			for _, cand := range layerResults {
				if seen[cand.Text] {
					continue
				}
				seen[cand.Text] = true
				results = append(results, cand)
			}
		}
	}

	sort.SliceStable(results, func(i, j int) bool {
		return candidate.Better(results[i], results[j])
	})

	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}

	return results
}
