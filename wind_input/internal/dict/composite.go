package dict

import (
	"sort"
	"sync"

	"github.com/huanfeng/wind_input/internal/candidate"
)

// CompositeDict 聚合词库
// 按优先级组合多个词库层，实现分层叠加查询
type CompositeDict struct {
	mu     sync.RWMutex
	layers []DictLayer // 按优先级排序（LayerType 小的在前）

	// Shadow 规则提供者（可选）
	shadowProvider ShadowProvider

	// 排序模式
	sortMode candidate.CandidateSortMode
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
	seen := make(map[string]bool)
	var results []candidate.Candidate

	for _, layer := range c.layers {
		var layerResults []candidate.Candidate
		if isPrefix {
			layerResults = layer.SearchPrefix(code, 0)
		} else {
			layerResults = layer.Search(code, 0)
		}

		for _, cand := range layerResults {
			if seen[cand.Text] {
				continue
			}
			seen[cand.Text] = true
			results = append(results, cand)
		}
	}

	// 2. 排序
	comparator := candidate.Better
	if c.sortMode == candidate.SortByNatural {
		comparator = candidate.BetterNatural
	}
	sort.Slice(results, func(i, j int) bool {
		return comparator(results[i], results[j])
	})

	// 3. 限制返回数量
	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}

	return results
}

// ApplyShadowPins 在已排序的候选列表上应用 Shadow 的 pin（位置固定）和 delete（隐藏）规则。
// 这是引擎 Phase 6 的统一拦截器，五笔和拼音共用。
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

	// 1. 构建 deleted 集合（单字不删）
	deletedSet := make(map[string]bool, len(rules.Deleted))
	for _, word := range rules.Deleted {
		if len([]rune(word)) > 1 {
			deletedSet[word] = true
		}
	}

	// 2. 过滤 deleted，同时记录 pinned 候选的原始信息
	pinnedWords := make(map[string]bool, len(rules.Pinned))
	for _, p := range rules.Pinned {
		pinnedWords[p.Word] = true
	}

	var unpinned []candidate.Candidate                  // 未被 pin 的候选
	pinnedCands := make(map[string]candidate.Candidate) // word → 候选信息
	for _, c := range candidates {
		if deletedSet[c.Text] {
			continue
		}
		if pinnedWords[c.Text] {
			pinnedCands[c.Text] = c
		} else {
			unpinned = append(unpinned, c)
		}
	}

	// 3. 按 pin 规则分配槽位（LIFO：数组前面的优先级高）
	// slots[position] = candidate
	slots := make(map[int]candidate.Candidate)
	usedPositions := make(map[int]bool)

	for _, pin := range rules.Pinned {
		cand, exists := pinnedCands[pin.Word]
		if !exists {
			continue // pin 的词不在候选列表中（词库变更后自然失效）
		}

		pos := pin.Position
		if pos < 0 {
			pos = 0
		}

		// 碰撞顺延：找到最近的空槽位
		for usedPositions[pos] {
			pos++
		}
		slots[pos] = cand
		usedPositions[pos] = true
	}

	// 4. 合并：pin 词插入指定位置，unpinned 填充剩余
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
	// 追加剩余 unpinned（pin position 超出范围时）
	for unpinnedIdx < len(unpinned) {
		result = append(result, unpinned[unpinnedIdx])
		unpinnedIdx++
	}

	return result
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
func (c *CompositeDict) LookupCommand(code string) []candidate.Candidate {
	c.mu.RLock()
	defer c.mu.RUnlock()

	for _, layer := range c.layers {
		if cl, ok := layer.(interface {
			SearchCommand(code string, limit int) []candidate.Candidate
		}); ok {
			results := cl.SearchCommand(code, 0)
			if len(results) > 0 {
				return results
			}
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

	sort.Slice(results, func(i, j int) bool {
		return candidate.Better(results[i], results[j])
	})

	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}

	return results
}
