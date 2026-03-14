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
func (c *CompositeDict) searchInternal(code string, limit int, isPrefix bool) []candidate.Candidate {
	// 1. 获取 Shadow 规则
	var shadowRules []ShadowRule
	deleted := make(map[string]bool)
	topped := make([]candidate.Candidate, 0)
	reweighted := make(map[string]int)

	if c.shadowProvider != nil {
		shadowRules = c.shadowProvider.GetShadowRules(code)
		for _, rule := range shadowRules {
			switch rule.Action {
			case ShadowActionDelete:
				deleted[rule.Word] = true
			case ShadowActionTop:
				// 置顶词稍后添加到结果最前面
				topped = append(topped, candidate.Candidate{
					Text:   rule.Word,
					Code:   rule.Code,
					Weight: 999999, // 置顶词使用最高权重
				})
			case ShadowActionReweight:
				reweighted[rule.Word] = rule.NewWeight
			}
		}
	}

	// 2. 遍历所有层收集候选词
	seen := make(map[string]bool)
	var results []candidate.Candidate

	// 先添加置顶词到 seen 集合
	for _, cand := range topped {
		seen[cand.Text] = true
	}

	for _, layer := range c.layers {
		var layerResults []candidate.Candidate
		if isPrefix {
			layerResults = layer.SearchPrefix(code, 0) // 不限制，后面统一限制
		} else {
			layerResults = layer.Search(code, 0)
		}

		for _, cand := range layerResults {
			// 跳过被删除的词
			if deleted[cand.Text] {
				continue
			}

			// 跳过已经出现过的词（上层优先）
			if seen[cand.Text] {
				continue
			}
			seen[cand.Text] = true

			// 应用权重调整
			if newWeight, ok := reweighted[cand.Text]; ok {
				cand.Weight = newWeight
			}

			results = append(results, cand)
		}
	}

	// 3. 合并置顶词和普通结果
	finalResults := append(topped, results...)

	// 4. 排序（置顶词已经在最前面，其余按排序模式排序）
	// 置顶词保持原顺序，非置顶词按配置的排序模式排序
	comparator := candidate.Better
	if c.sortMode == candidate.SortByNatural {
		comparator = candidate.BetterNatural
	}

	if len(topped) > 0 && len(results) > 0 {
		// 只对非置顶部分排序
		nonToppedStart := len(topped)
		nonTopped := finalResults[nonToppedStart:]
		sort.Slice(nonTopped, func(i, j int) bool {
			return comparator(nonTopped[i], nonTopped[j])
		})
	} else if len(topped) == 0 {
		// 没有置顶词，全部排序
		sort.Slice(finalResults, func(i, j int) bool {
			return comparator(finalResults[i], finalResults[j])
		})
	}

	// 5. 限制返回数量
	if limit > 0 && len(finalResults) > limit {
		finalResults = finalResults[:limit]
	}

	return finalResults
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
// 实现 dict.Dict 接口，使 CompositeDict 可被拼音引擎使用
// ============================================================

// Lookup 实现 dict.Dict 接口
func (c *CompositeDict) Lookup(pinyin string) []candidate.Candidate {
	results := c.Search(pinyin, 0)
	// log.Printf("[CompositeDict] Lookup: pinyin=%q results=%d", pinyin, len(results))
	return results
}

// LookupPhrase 实现 dict.Dict 接口
// 将音节列表拼接后查询
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
