package dict

import (
	"sort"
	"strings"

	"github.com/huanfeng/wind_input/internal/candidate"
)

// CodeTableLayer 将 CodeTable 适配为 DictLayer
type CodeTableLayer struct {
	name      string
	layerType LayerType
	codeTable *CodeTable
}

// NewCodeTableLayer 创建 CodeTable 适配器
func NewCodeTableLayer(name string, layerType LayerType, ct *CodeTable) *CodeTableLayer {
	return &CodeTableLayer{
		name:      name,
		layerType: layerType,
		codeTable: ct,
	}
}

// Name 返回层名称
func (l *CodeTableLayer) Name() string {
	return l.name
}

// Type 返回层类型
func (l *CodeTableLayer) Type() LayerType {
	return l.layerType
}

// Search 精确查询
func (l *CodeTableLayer) Search(code string, limit int) []candidate.Candidate {
	results := l.codeTable.Lookup(code)

	// 排序
	sorted := make([]candidate.Candidate, len(results))
	copy(sorted, results)
	sort.SliceStable(sorted, func(i, j int) bool {
		if sorted[i].Weight != sorted[j].Weight {
			return sorted[i].Weight > sorted[j].Weight
		}
		return sorted[i].NaturalOrder < sorted[j].NaturalOrder
	})

	// 限制数量
	if limit > 0 && len(sorted) > limit {
		sorted = sorted[:limit]
	}

	return sorted
}

// SearchPrefix 前缀查询
func (l *CodeTableLayer) SearchPrefix(prefix string, limit int) []candidate.Candidate {
	results := l.codeTable.LookupPrefix(prefix)

	// 排序
	sorted := make([]candidate.Candidate, len(results))
	copy(sorted, results)
	sort.SliceStable(sorted, func(i, j int) bool {
		if sorted[i].Weight != sorted[j].Weight {
			return sorted[i].Weight > sorted[j].Weight
		}
		return sorted[i].NaturalOrder < sorted[j].NaturalOrder
	})

	// 限制数量
	if limit > 0 && len(sorted) > limit {
		sorted = sorted[:limit]
	}

	return sorted
}

// GetCodeTable 获取底层 CodeTable（用于特殊操作）
func (l *CodeTableLayer) GetCodeTable() *CodeTable {
	return l.codeTable
}

// SimpleDictLayer 将 SimpleDict 适配为 DictLayer
type SimpleDictLayer struct {
	name       string
	layerType  LayerType
	simpleDict *SimpleDict
}

// NewSimpleDictLayer 创建 SimpleDict 适配器
func NewSimpleDictLayer(name string, layerType LayerType, sd *SimpleDict) *SimpleDictLayer {
	return &SimpleDictLayer{
		name:       name,
		layerType:  layerType,
		simpleDict: sd,
	}
}

// Name 返回层名称
func (l *SimpleDictLayer) Name() string {
	return l.name
}

// Type 返回层类型
func (l *SimpleDictLayer) Type() LayerType {
	return l.layerType
}

// Search 精确查询
func (l *SimpleDictLayer) Search(code string, limit int) []candidate.Candidate {
	results := l.simpleDict.Lookup(code)

	// 排序
	sorted := make([]candidate.Candidate, len(results))
	copy(sorted, results)
	sort.SliceStable(sorted, func(i, j int) bool {
		if sorted[i].Weight != sorted[j].Weight {
			return sorted[i].Weight > sorted[j].Weight
		}
		return sorted[i].NaturalOrder < sorted[j].NaturalOrder
	})

	// 限制数量
	if limit > 0 && len(sorted) > limit {
		sorted = sorted[:limit]
	}

	return sorted
}

// SearchPrefix 前缀查询
// SimpleDict 主要用于拼音，前缀匹配需要遍历
func (l *SimpleDictLayer) SearchPrefix(prefix string, limit int) []candidate.Candidate {
	prefix = strings.ToLower(prefix)
	var results []candidate.Candidate

	// 遍历 entries 查找前缀匹配
	for key, candidates := range l.simpleDict.GetEntries() {
		if strings.HasPrefix(key, prefix) {
			results = append(results, candidates...)
		}
	}

	// 排序
	sort.SliceStable(results, func(i, j int) bool {
		if results[i].Weight != results[j].Weight {
			return results[i].Weight > results[j].Weight
		}
		return results[i].NaturalOrder < results[j].NaturalOrder
	})

	// 限制数量
	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}

	return results
}

// GetSimpleDict 获取底层 SimpleDict（用于特殊操作）
func (l *SimpleDictLayer) GetSimpleDict() *SimpleDict {
	return l.simpleDict
}

// LookupPhrase 查找短语（SimpleDict 特有方法）
func (l *SimpleDictLayer) LookupPhrase(syllables []string) []candidate.Candidate {
	return l.simpleDict.LookupPhrase(syllables)
}
