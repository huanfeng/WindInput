package candidate

import "strings"

// FilterCandidates 根据过滤模式筛选候选词
func FilterCandidates(candidates []Candidate, mode string) []Candidate {
	if len(candidates) == 0 {
		return candidates
	}

	mode = strings.ToLower(strings.TrimSpace(mode))
	switch mode {
	case "general":
		return filterCommonOnly(candidates)
	case "gb18030":
		return candidates
	case "smart":
		return filterSmart(candidates)
	default:
		return candidates
	}
}

// filterSmart 智能过滤：仅在同编码内部进行生僻字过滤
func filterSmart(candidates []Candidate) []Candidate {
	// 1. 识别哪些编码拥有常用字或指令（短语不计入：避免短语污染同编码下的码表字过滤判定）
	hasCommon := make(map[string]bool)
	for _, c := range candidates {
		if c.IsPhrase {
			continue
		}
		if c.IsCommon || c.IsCommand || c.IsGroup {
			hasCommon[c.Code] = true
		}
	}

	// 2. 过滤：仅当同编码下存在常用字时，才过滤掉该编码下的生僻字
	result := make([]Candidate, 0, len(candidates))
	for _, c := range candidates {
		// 满足以下任一条件即保留：
		// - 自身是常用字、短语、指令或组
		// - 同编码下没有任何常用字/指令（孤儿词条，必须保留）
		if c.IsCommon || c.IsPhrase || c.IsCommand || c.IsGroup || !hasCommon[c.Code] {
			result = append(result, c)
		}
	}
	return result
}

func filterCommonOnly(candidates []Candidate) []Candidate {
	result := make([]Candidate, 0, len(candidates))
	for _, c := range candidates {
		if c.IsCommon || c.IsPhrase || c.IsCommand || c.IsGroup {
			result = append(result, c)
		}
	}
	return result
}
