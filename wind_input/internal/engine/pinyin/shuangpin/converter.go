package shuangpin

import "strings"

// ConvertResult 双拼→全拼转换结果
type ConvertResult struct {
	// FullPinyin 转换后的完整全拼字符串（如 "nihao"）
	FullPinyin string

	// Syllables 已完成的音节列表
	Syllables []ConvertedSyllable

	// PartialInitial 未配对的最后一个键（等待韵母输入），空字符串表示无
	PartialInitial string

	// PartialKey 未配对的原始按键
	PartialKey byte

	// HasPartial 是否有未完成的输入
	HasPartial bool

	// PositionMap 全拼位置→双拼位置的映射（用于 ConsumedLength 回映射）
	// PositionMap[i] 表示全拼中第 i 个字节对应的双拼原始字节偏移
	PositionMap []int

	// PreeditDisplay 预编辑区显示文本（全拼 + 分隔符），如 "ni'hao"
	PreeditDisplay string
}

// ConvertedSyllable 一个转换后的音节
type ConvertedSyllable struct {
	Pinyin  string // 全拼文本（如 "hao"）
	SPStart int    // 在双拼原始输入中的起始位置
	SPEnd   int    // 在双拼原始输入中的结束位置（不包含）
	FPStart int    // 在全拼输出中的起始位置
	FPEnd   int    // 在全拼输出中的结束位置
}

// Converter 双拼→全拼转换器
type Converter struct {
	scheme       *Scheme
	validPinyins map[string]bool // 合法拼音音节集合
}

// NewConverter 创建双拼转换器
func NewConverter(scheme *Scheme) *Converter {
	c := &Converter{
		scheme:       scheme,
		validPinyins: buildValidPinyinSet(),
	}
	return c
}

// GetScheme 获取当前方案
func (c *Converter) GetScheme() *Scheme {
	return c.scheme
}

// SetScheme 切换方案
func (c *Converter) SetScheme(scheme *Scheme) {
	c.scheme = scheme
}

// Convert 将双拼键序列转换为全拼
// input 为小写字母序列（如 "nihc" 在小鹤方案下）
func (c *Converter) Convert(input string) *ConvertResult {
	result := &ConvertResult{
		Syllables:   make([]ConvertedSyllable, 0, len(input)/2+1),
		PositionMap: make([]int, 0, len(input)*2),
	}

	if len(input) == 0 {
		return result
	}

	input = strings.ToLower(input)

	var fullPinyinBuilder strings.Builder
	fullPinyinBuilder.Grow(len(input) * 2) // 预分配

	var preeditBuilder strings.Builder
	preeditBuilder.Grow(len(input) * 3)

	fpPos := 0 // 全拼当前位置

	i := 0
	for i < len(input) {
		key1 := input[i]

		if i+1 >= len(input) {
			// 奇数键：最后一个键是未配对的（partial）
			initial, hasInitial := c.scheme.InitialMap[key1]
			if hasInitial {
				result.PartialInitial = initial
			} else {
				result.PartialInitial = string(key1)
			}
			result.PartialKey = key1
			result.HasPartial = true

			// 将 partial 的声母写入全拼（用于前缀匹配）
			partialStr := result.PartialInitial
			if preeditBuilder.Len() > 0 {
				preeditBuilder.WriteByte('\'')
			}
			preeditBuilder.WriteString(partialStr)
			fullPinyinBuilder.WriteString(partialStr)
			for range partialStr {
				result.PositionMap = append(result.PositionMap, i)
			}
			break
		}

		key2 := input[i+1]

		// 尝试转换这一对键为全拼音节
		syllables := c.convertPair(key1, key2)

		if len(syllables) > 0 {
			// 取第一个有效音节（通常只有一个）
			// 如果有多个，全部作为候选（通过全拼引擎的多切分来处理）
			bestSyllable := syllables[0]

			if preeditBuilder.Len() > 0 {
				preeditBuilder.WriteByte('\'')
			}
			preeditBuilder.WriteString(bestSyllable)

			fullPinyinBuilder.WriteString(bestSyllable)

			converted := ConvertedSyllable{
				Pinyin:  bestSyllable,
				SPStart: i,
				SPEnd:   i + 2,
				FPStart: fpPos,
				FPEnd:   fpPos + len(bestSyllable),
			}
			result.Syllables = append(result.Syllables, converted)

			// 构建位置映射：全拼中的每个字节都映射回双拼的 2 字节位置
			for j := 0; j < len(bestSyllable); j++ {
				if j < len(bestSyllable)/2 {
					result.PositionMap = append(result.PositionMap, i)
				} else {
					result.PositionMap = append(result.PositionMap, i+1)
				}
			}

			fpPos += len(bestSyllable)
		} else {
			// 无法匹配：将两个键原样保留
			s := string([]byte{key1, key2})
			if preeditBuilder.Len() > 0 {
				preeditBuilder.WriteByte('\'')
			}
			preeditBuilder.WriteString(s)
			fullPinyinBuilder.WriteString(s)

			result.PositionMap = append(result.PositionMap, i)
			result.PositionMap = append(result.PositionMap, i+1)
			fpPos += 2
		}

		i += 2
	}

	result.FullPinyin = fullPinyinBuilder.String()
	result.PreeditDisplay = preeditBuilder.String()
	return result
}

// MapConsumedLength 将全拼 ConsumedLength 回映射为双拼 ConsumedLength
// fpConsumed: 全拼引擎报告的已消耗字节数
// 返回：在双拼原始输入中对应的字节数
func (r *ConvertResult) MapConsumedLength(fpConsumed int) int {
	if fpConsumed <= 0 {
		return 0
	}

	// 优先通过音节边界精确映射
	fpEnd := 0
	for _, s := range r.Syllables {
		fpEnd += len(s.Pinyin)
		if fpEnd >= fpConsumed {
			return s.SPEnd
		}
	}

	// Fallback: 使用位置映射表
	// 覆盖所有音节边界无法精确映射的场景：partial、无效键对（简拼）等
	if fpConsumed > len(r.PositionMap) {
		fpConsumed = len(r.PositionMap)
	}
	if fpConsumed > 0 {
		return r.PositionMap[fpConsumed-1] + 1
	}
	return 0
}

// convertPair 转换一对键为全拼音节列表
// 返回所有可能的合法拼音音节
func (c *Converter) convertPair(key1, key2 byte) []string {
	var results []string

	// 1. 检查零声母
	if zeroSyllables, ok := c.scheme.ZeroInitialKeys[key1]; ok {
		// key1 是零声母的伪声母键
		finals := c.scheme.FinalMap[key2]
		for _, f := range finals {
			// 零声母+韵母
			if c.validPinyins[f] {
				results = append(results, f)
			}
		}
		// 也检查零声母音节列表中是否有匹配的
		for _, syllable := range zeroSyllables {
			matched := false
			// 直接表音匹配：key1+key2 本身拼成零声母音节
			// 如 "ai": a+i，双拼规则就是直接用表音字母输入
			if syllable == string(key1)+string(key2) {
				matched = true
			} else if c.matchesFinal(syllable, key2) {
				// 通过 FinalMap 反查：如 "ai": a+d(ai的韵母键) 在小鹤方案下
				matched = true
			}
			if matched {
				found := false
				for _, r := range results {
					if r == syllable {
						found = true
						break
					}
				}
				if !found {
					results = append(results, syllable)
				}
			}
		}
	}

	// 2. 常规声母+韵母
	initial, hasInitial := c.scheme.InitialMap[key1]
	if hasInitial {
		finals := c.scheme.FinalMap[key2]
		for _, f := range finals {
			syllable := initial + f
			// 特殊处理：ü 相关
			syllable = normalizePinyin(syllable)
			if c.validPinyins[syllable] {
				found := false
				for _, r := range results {
					if r == syllable {
						found = true
						break
					}
				}
				if !found {
					results = append(results, syllable)
				}
			}
		}
	}

	// 3. 零声母特殊处理：单韵母重复键（aa→a, oo→o, ee→e）
	if key1 == key2 {
		single := string(key1)
		if c.validPinyins[single] {
			found := false
			for _, r := range results {
				if r == single {
					found = true
					break
				}
			}
			if !found {
				results = append(results, single)
			}
		}
	}

	return results
}

// matchesFinal 检查一个完整音节是否匹配给定的韵母键
func (c *Converter) matchesFinal(syllable string, finalKey byte) bool {
	finals, ok := c.scheme.FinalMap[finalKey]
	if !ok {
		return false
	}
	// 提取音节的韵母部分
	syllableFinal := extractFinal(syllable)
	for _, f := range finals {
		if f == syllableFinal {
			return true
		}
	}
	return false
}

// extractFinal 提取拼音音节的韵母部分
func extractFinal(syllable string) string {
	// 零声母音节：整个就是韵母
	for _, initial := range []string{"zh", "ch", "sh", "b", "p", "m", "f", "d", "t", "n", "l", "g", "k", "h", "j", "q", "x", "r", "z", "c", "s", "y", "w"} {
		if strings.HasPrefix(syllable, initial) && len(syllable) > len(initial) {
			return syllable[len(initial):]
		}
	}
	// 零声母，整个音节就是韵母
	return syllable
}

// normalizePinyin 标准化拼音（处理 ü 相关的特殊规则）
func normalizePinyin(pinyin string) string {
	// lv → lü, nv → nü（v 表示 ü）
	// jv → ju, qv → qu, xv → xu, yv → yu（j/q/x/y 后的 v 实际是 u）
	switch {
	case strings.HasPrefix(pinyin, "j") || strings.HasPrefix(pinyin, "q") ||
		strings.HasPrefix(pinyin, "x") || strings.HasPrefix(pinyin, "y"):
		// j/q/x/y + v → u, ve → ue, vn → un
		pinyin = strings.Replace(pinyin, "ve", "ue", 1)
		pinyin = strings.Replace(pinyin, "vn", "un", 1)
		if strings.Contains(pinyin, "v") {
			pinyin = strings.Replace(pinyin, "v", "u", 1)
		}
	}
	return pinyin
}

// buildValidPinyinSet 构建合法拼音音节集合
func buildValidPinyinSet() map[string]bool {
	set := make(map[string]bool, len(validPinyinSyllables))
	for _, s := range validPinyinSyllables {
		set[s] = true
	}
	return set
}

// validPinyinSyllables 所有合法的拼音音节（约 410 个）
// 来源：汉语拼音方案标准音节表
var validPinyinSyllables = []string{
	// 零声母
	"a", "o", "e", "ai", "ei", "ao", "ou", "an", "en", "ang", "eng", "er",

	// b
	"ba", "bo", "bi", "bu", "bai", "bei", "bao", "ban", "ben", "bin",
	"bang", "beng", "bing", "bie", "biao", "bian",

	// p
	"pa", "po", "pi", "pu", "pai", "pei", "pao", "pou", "pan", "pen", "pin",
	"pang", "peng", "ping", "pie", "piao", "pian",

	// m
	"ma", "mo", "me", "mi", "mu", "mai", "mei", "mao", "mou", "man", "men", "min",
	"mang", "meng", "ming", "mie", "miao", "mian", "miu",

	// f
	"fa", "fo", "fu", "fei", "fou", "fan", "fen", "fang", "feng",

	// d
	"da", "de", "di", "du", "dai", "dei", "dao", "dou", "dan", "den",
	"dang", "deng", "ding", "die", "diao", "dian", "diu",
	"duo", "dui", "duan", "dun", "dong",

	// t
	"ta", "te", "ti", "tu", "tai", "tei", "tao", "tou", "tan",
	"tang", "teng", "ting", "tie", "tiao", "tian",
	"tuo", "tui", "tuan", "tun", "tong",

	// n
	"na", "ne", "ni", "nu", "nv", "nai", "nei", "nao", "nou", "nan", "nen", "nin",
	"nang", "neng", "ning", "nie", "niao", "nian", "niang", "niu",
	"nuo", "nuan", "nun", "nong", "nve",

	// l
	"la", "le", "li", "lu", "lv", "lai", "lei", "lao", "lou", "lan", "lin",
	"lang", "leng", "ling", "lia", "lie", "liao", "lian", "liang", "liu",
	"luo", "luan", "lun", "long", "lve",

	// g
	"ga", "ge", "gu", "gai", "gei", "gao", "gou", "gan", "gen",
	"gang", "geng", "gua", "guo", "gui", "guai", "guan", "gun", "guang", "gong",

	// k
	"ka", "ke", "ku", "kai", "kei", "kao", "kou", "kan", "ken",
	"kang", "keng", "kua", "kuo", "kui", "kuai", "kuan", "kun", "kuang", "kong",

	// h
	"ha", "he", "hu", "hai", "hei", "hao", "hou", "han", "hen",
	"hang", "heng", "hua", "huo", "hui", "huai", "huan", "hun", "huang", "hong",

	// j
	"ji", "ju", "jia", "jie", "jiao", "jiu", "jian", "jin", "jiang", "jing",
	"jiong", "jue", "jun", "juan",

	// q
	"qi", "qu", "qia", "qie", "qiao", "qiu", "qian", "qin", "qiang", "qing",
	"qiong", "que", "qun", "quan",

	// x
	"xi", "xu", "xia", "xie", "xiao", "xiu", "xian", "xin", "xiang", "xing",
	"xiong", "xue", "xun", "xuan",

	// zh
	"zha", "zhe", "zhi", "zhu", "zhai", "zhei", "zhao", "zhou", "zhan", "zhen",
	"zhang", "zheng", "zhua", "zhuo", "zhui", "zhuai", "zhuan", "zhun", "zhuang", "zhong",

	// ch
	"cha", "che", "chi", "chu", "chai", "chao", "chou", "chan", "chen",
	"chang", "cheng", "chua", "chuo", "chui", "chuai", "chuan", "chun", "chuang", "chong",

	// sh
	"sha", "she", "shi", "shu", "shai", "shei", "shao", "shou", "shan", "shen",
	"shang", "sheng", "shua", "shuo", "shui", "shuai", "shuan", "shun", "shuang",

	// r
	"re", "ri", "ru", "rao", "rou", "ran", "ren",
	"rang", "reng", "rua", "ruo", "rui", "ruan", "run", "rong",

	// z
	"za", "ze", "zi", "zu", "zai", "zei", "zao", "zou", "zan", "zen",
	"zang", "zeng", "zuo", "zui", "zuan", "zun", "zong",

	// c
	"ca", "ce", "ci", "cu", "cai", "cao", "cou", "can", "cen",
	"cang", "ceng", "cuo", "cui", "cuan", "cun", "cong",

	// s
	"sa", "se", "si", "su", "sai", "sao", "sou", "san", "sen",
	"sang", "seng", "suo", "sui", "suan", "sun", "song",

	// y
	"ya", "ye", "yi", "yo", "yu", "yao", "you", "yan", "yin",
	"yang", "ying", "yue", "yun", "yuan", "yong",

	// w
	"wa", "wo", "wu", "wai", "wei", "wan", "wen",
	"wang", "weng",
}
