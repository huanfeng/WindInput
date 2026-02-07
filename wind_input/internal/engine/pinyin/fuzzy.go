package pinyin

import "strings"

// FuzzyConfig 模糊拼音配置
type FuzzyConfig struct {
	ZhZ   bool // zh ↔ z
	ChC   bool // ch ↔ c
	ShS   bool // sh ↔ s
	NL    bool // n ↔ l
	AnAng bool // an ↔ ang（韵母）
	EnEng bool // en ↔ eng（韵母）
	InIng bool // in ↔ ing（韵母）
}

// Enabled 是否启用了任何模糊拼音
func (fc *FuzzyConfig) Enabled() bool {
	if fc == nil {
		return false
	}
	return fc.ZhZ || fc.ChC || fc.ShS || fc.NL ||
		fc.AnAng || fc.EnEng || fc.InIng
}

// 声母模糊对
var initialPairs = [][2]string{
	{"zh", "z"},
	{"ch", "c"},
	{"sh", "s"},
	{"n", "l"},
}

// 韵母模糊对
var finalPairs = [][2]string{
	{"ang", "an"},
	{"eng", "en"},
	{"ing", "in"},
}

// splitInitialFinal 将音节拆分为声母和韵母
func splitInitialFinal(syllable string) (initial, final string) {
	// 双字母声母优先
	doubleInitials := []string{"zh", "ch", "sh"}
	for _, di := range doubleInitials {
		if strings.HasPrefix(syllable, di) {
			return di, syllable[len(di):]
		}
	}
	// 单字母声母
	singleInitials := "bpmfdtnlgkhjqxrzcsyw"
	if len(syllable) > 0 && strings.ContainsRune(singleInitials, rune(syllable[0])) {
		return syllable[:1], syllable[1:]
	}
	// 零声母
	return "", syllable
}

// Variants 生成一个音节的所有模糊变体（不含原始音节）
func (fc *FuzzyConfig) Variants(syllable string) []string {
	if fc == nil || !fc.Enabled() {
		return nil
	}

	initial, final := splitInitialFinal(syllable)

	var altInitials []string
	var altFinals []string

	// 声母替换
	altInitials = append(altInitials, initial) // 原始声母
	if fc.ZhZ {
		if initial == "zh" {
			altInitials = append(altInitials, "z")
		} else if initial == "z" {
			altInitials = append(altInitials, "zh")
		}
	}
	if fc.ChC {
		if initial == "ch" {
			altInitials = append(altInitials, "c")
		} else if initial == "c" {
			altInitials = append(altInitials, "ch")
		}
	}
	if fc.ShS {
		if initial == "sh" {
			altInitials = append(altInitials, "s")
		} else if initial == "s" {
			altInitials = append(altInitials, "sh")
		}
	}
	if fc.NL {
		if initial == "n" {
			altInitials = append(altInitials, "l")
		} else if initial == "l" {
			altInitials = append(altInitials, "n")
		}
	}

	// 韵母替换
	altFinals = append(altFinals, final) // 原始韵母
	if fc.AnAng {
		if strings.HasSuffix(final, "ang") {
			altFinals = append(altFinals, final[:len(final)-3]+"an")
		} else if strings.HasSuffix(final, "an") && !strings.HasSuffix(final, "uan") {
			// "an" → "ang"，但排除 "uan" 中的 "an"（避免 "uan"→"uang" 由此规则覆盖）
			altFinals = append(altFinals, final+"g")
		}
	}
	if fc.EnEng {
		if strings.HasSuffix(final, "eng") {
			altFinals = append(altFinals, final[:len(final)-3]+"en")
		} else if strings.HasSuffix(final, "en") && !strings.HasSuffix(final, "uen") {
			altFinals = append(altFinals, final+"g")
		}
	}
	if fc.InIng {
		if strings.HasSuffix(final, "ing") {
			altFinals = append(altFinals, final[:len(final)-3]+"in")
		} else if strings.HasSuffix(final, "in") && !strings.HasSuffix(final, "uin") {
			altFinals = append(altFinals, final+"g")
		}
	}

	// 组合所有声母×韵母变体，排除原始音节
	var variants []string
	seen := make(map[string]bool)
	seen[syllable] = true

	for _, ini := range altInitials {
		for _, fin := range altFinals {
			v := ini + fin
			if v == syllable || seen[v] {
				continue
			}
			// 验证是否为合法拼音
			if isValidPinyin(v) {
				seen[v] = true
				variants = append(variants, v)
			}
		}
	}

	return variants
}

// ExpandCode 将拼音编码（可能包含多音节）展开为模糊变体列表
// syllables 是已经切分好的音节列表
// 返回所有可能的编码组合（不含原始编码）
func (fc *FuzzyConfig) ExpandCode(syllables []string) []string {
	if fc == nil || !fc.Enabled() || len(syllables) == 0 {
		return nil
	}

	// 为每个音节收集其变体（含原始值）
	allOptions := make([][]string, len(syllables))
	hasAnyVariant := false
	for i, s := range syllables {
		variants := fc.Variants(s)
		allOptions[i] = append([]string{s}, variants...)
		if len(variants) > 0 {
			hasAnyVariant = true
		}
	}

	if !hasAnyVariant {
		return nil
	}

	// 笛卡尔积，限制总量
	original := strings.Join(syllables, "")
	var results []string
	maxResults := 16

	var expand func(depth int, parts []string)
	expand = func(depth int, parts []string) {
		if len(results) >= maxResults {
			return
		}
		if depth == len(allOptions) {
			code := strings.Join(parts, "")
			if code != original {
				results = append(results, code)
			}
			return
		}
		for _, opt := range allOptions[depth] {
			expand(depth+1, append(parts, opt))
		}
	}
	expand(0, nil)

	return results
}

// validPinyinSet 合法拼音集合（延迟初始化）
var validPinyinSet map[string]bool

// isValidPinyin 检查是否为合法拼音音节
func isValidPinyin(s string) bool {
	if validPinyinSet == nil {
		validPinyinSet = make(map[string]bool, len(allSyllables))
		for _, sy := range allSyllables {
			validPinyinSet[sy] = true
		}
	}
	return validPinyinSet[s]
}
