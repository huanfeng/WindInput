package s2t

import "github.com/huanfeng/wind_input/pkg/config"

// Chain 返回给定变体的转换链；每个步骤是一组词典（"group"）。
//
// OpenCC 语义：每步内的多个词典视作一个 group，扫描输入时在所有 group 成员上
// 各取最长前缀匹配，跨成员选最长一个；前一步的输出作为后一步输入。
//
// 例如 s2twp：
//
//  1. {STPhrases, STCharacters}     -- 简->繁；词级与字级共同竞争最长匹配
//  2. {TWPhrases}                   -- 繁->台湾习惯词（如 軟件 → 軟體）
//  3. {TWVariants}                  -- 字形微调
func Chain(v config.S2TVariant) [][]string {
	switch v {
	case config.S2TStandard:
		return [][]string{
			{"STPhrases", "STCharacters"},
		}
	case config.S2TTaiwan:
		return [][]string{
			{"STPhrases", "STCharacters"},
			{"TWVariants"},
		}
	case config.S2TTaiwanPhrase:
		return [][]string{
			{"STPhrases", "STCharacters"},
			{"TWPhrases"},
			{"TWVariants"},
		}
	case config.S2THongKong:
		return [][]string{
			{"STPhrases", "STCharacters"},
			{"HKVariants"},
		}
	default:
		return [][]string{
			{"STPhrases", "STCharacters"},
		}
	}
}

// AllRequiredDicts 返回所有变体可能用到的词典名集合（去重）。
func AllRequiredDicts() []string {
	return []string{
		"STCharacters",
		"STPhrases",
		"TWVariants",
		"TWPhrases",
		"HKVariants",
	}
}
