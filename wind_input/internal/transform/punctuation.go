// Package transform provides text transformation utilities
package transform

// 中英文标点映射表
var (
	// 英文标点 -> 中文标点 (单字符映射)
	englishToChinesePunct = map[rune]rune{
		',':  '\uFF0C', // 逗号 ，
		'.':  '\u3002', // 句号 。
		'?':  '\uFF1F', // 问号 ？
		'!':  '\uFF01', // 感叹号 ！
		':':  '\uFF1A', // 冒号 ：
		';':  '\uFF1B', // 分号 ；
		'(':  '\uFF08', // 左括号 （
		')':  '\uFF09', // 右括号 ）
		'[':  '\u3010', // 左方括号 【
		']':  '\u3011', // 右方括号 】
		'{':  '\uFF5B', // 左花括号 ｛
		'}':  '\uFF5D', // 右花括号 ｝
		'<':  '\u300A', // 左书名号 《
		'>':  '\u300B', // 右书名号 》
		'~':  '\uFF5E', // 波浪号 ～
		'$':  '\uFFE5', // 人民币符号 ￥
		'`':  '\u00B7', // 反引号 -> 间隔号 ·
		'\\': '\u3001', // 反斜杠 -> 顿号 、
	}

	// 英文标点 -> 中文标点字符串 (多字符映射)
	englishToChinesePunctStr = map[rune]string{
		'^': "\u2026\u2026", // 省略号 ……
		'_': "\u2014\u2014", // 破折号 ——
	}

	// 中文标点 -> 英文标点
	chineseToEnglishPunct = map[rune]rune{
		'\uFF0C': ',',  // ，
		'\u3002': '.',  // 。
		'\uFF1F': '?',  // ？
		'\uFF01': '!',  // ！
		'\uFF1A': ':',  // ：
		'\uFF1B': ';',  // ；
		'\uFF08': '(',  // （
		'\uFF09': ')',  // ）
		'\u3010': '[',  // 【
		'\u3011': ']',  // 】
		'\uFF5B': '{',  // ｛
		'\uFF5D': '}',  // ｝
		'\u300A': '<',  // 《
		'\u300B': '>',  // 》
		'\u2018': '\'', // '
		'\u2019': '\'', // '
		'\u201C': '"',  // "
		'\u201D': '"',  // "
		'\u00B7': '`',  // · -> `
		'\u3001': '\\', // 、 -> \
		'\uFF5E': '~',  // ～
		'\uFFE5': '$',  // ￥
	}

	// 左引号
	chineseSingleQuoteLeft  = '\u2018' // '
	chineseSingleQuoteRight = '\u2019' // '
	chineseDoubleQuoteLeft  = '\u201C' // "
	chineseDoubleQuoteRight = '\u201D' // "
)

// PunctuationConverter handles punctuation conversion with state
type PunctuationConverter struct {
	// 引号状态: true=下次输出左引号, false=下次输出右引号
	singleQuoteLeft bool
	doubleQuoteLeft bool
	// 已配对的引号：这些引号始终输出左引号，由配对逻辑负责补全右引号
	pairedSingleQuote bool
	pairedDoubleQuote bool
	// 自定义标点映射
	customEnabled  bool
	customMappings map[string][]string // key=源字符, value=[中文半角,英文全角,中文全角]
}

// NewPunctuationConverter creates a new punctuation converter
func NewPunctuationConverter() *PunctuationConverter {
	return &PunctuationConverter{
		singleQuoteLeft: true,
		doubleQuoteLeft: true,
	}
}

// SetPairedQuotes 设置哪些引号由配对逻辑接管（跳过交替输出）
// 当引号在配对表中时，始终输出左引号，配对追踪器会自动补全右引号
func (c *PunctuationConverter) SetPairedQuotes(singlePaired, doublePaired bool) {
	c.pairedSingleQuote = singlePaired
	c.pairedDoubleQuote = doublePaired
}

// SetCustomMappings 设置自定义标点映射
func (c *PunctuationConverter) SetCustomMappings(enabled bool, mappings map[string][]string) {
	c.customEnabled = enabled
	c.customMappings = mappings
}

// LookupCustom 查找自定义映射。colIdx: 0=中文半角, 1=英文全角, 2=中文全角
// 对于引号，根据当前交替状态选择 "1/"2 或 '1/'2 作为 key
// 找到非空结果时切换引号状态并返回 (result, true)；未找到返回 ("", false) 且不切换状态
func (c *PunctuationConverter) LookupCustom(r rune, colIdx int) (string, bool) {
	if !c.customEnabled || c.customMappings == nil {
		return "", false
	}

	var key string
	isQuote := false
	switch r {
	case '"':
		isQuote = true
		if c.doubleQuoteLeft {
			key = `"1`
		} else {
			key = `"2`
		}
	case '\'':
		isQuote = true
		if c.singleQuoteLeft {
			key = `'1`
		} else {
			key = `'2`
		}
	default:
		key = string(r)
	}

	vals, ok := c.customMappings[key]
	if !ok || colIdx >= len(vals) || vals[colIdx] == "" {
		return "", false
	}

	// 找到自定义映射，切换引号状态
	if isQuote {
		switch r {
		case '"':
			c.doubleQuoteLeft = !c.doubleQuoteLeft
		case '\'':
			c.singleQuoteLeft = !c.singleQuoteLeft
		}
	}

	return vals[colIdx], true
}

// Reset resets the converter state (e.g., when switching modes)
func (c *PunctuationConverter) Reset() {
	c.singleQuoteLeft = true
	c.doubleQuoteLeft = true
}

// ToChinesePunct converts English punctuation to Chinese punctuation
// 英文标点转中文标点
func (c *PunctuationConverter) ToChinesePunct(r rune) (rune, bool) {
	// 处理成对标点（引号）
	switch r {
	case '\'':
		if c.pairedSingleQuote {
			// 配对模式：始终输出左引号，由配对追踪器补全右引号
			return chineseSingleQuoteLeft, true
		}
		if c.singleQuoteLeft {
			c.singleQuoteLeft = false
			return chineseSingleQuoteLeft, true
		} else {
			c.singleQuoteLeft = true
			return chineseSingleQuoteRight, true
		}
	case '"':
		if c.pairedDoubleQuote {
			return chineseDoubleQuoteLeft, true
		}
		if c.doubleQuoteLeft {
			c.doubleQuoteLeft = false
			return chineseDoubleQuoteLeft, true
		} else {
			c.doubleQuoteLeft = true
			return chineseDoubleQuoteRight, true
		}
	}

	// 普通标点映射
	if chinese, ok := englishToChinesePunct[r]; ok {
		return chinese, true
	}

	return r, false
}

// ToChinesePunctStr converts English punctuation to Chinese punctuation string
// 用于处理多字符结果（如省略号、破折号）
func (c *PunctuationConverter) ToChinesePunctStr(r rune) (string, bool) {
	// 先检查多字符映射
	if str, ok := englishToChinesePunctStr[r]; ok {
		return str, true
	}
	// 再检查单字符映射
	if ch, ok := c.ToChinesePunct(r); ok {
		return string(ch), true
	}
	return string(r), false
}

// PeekChineseStr 预测 ToChinesePunctStr(r) 的输出但**不修改任何状态**（不翻转引号
// 左右交替）。覆盖多字符标点（^→……、_→——）、引号（按当前左右状态/配对状态预测）、
// 以及普通单字符标点。供智能符号模式无副作用地预判按键将产生的中文标点串。
func (c *PunctuationConverter) PeekChineseStr(r rune) (string, bool) {
	if str, ok := englishToChinesePunctStr[r]; ok {
		return str, true
	}
	switch r {
	case '\'':
		if c.pairedSingleQuote || c.singleQuoteLeft {
			return string(chineseSingleQuoteLeft), true
		}
		return string(chineseSingleQuoteRight), true
	case '"':
		if c.pairedDoubleQuote || c.doubleQuoteLeft {
			return string(chineseDoubleQuoteLeft), true
		}
		return string(chineseDoubleQuoteRight), true
	}
	if ch, ok := englishToChinesePunct[r]; ok {
		return string(ch), true
	}
	return "", false
}

// RevertLastQuote 把引号的左右交替状态回退一步。智能符号替换吃掉一个引号后调用，
// 抵消 convertPunct 已做出的翻转，使下一次同引号仍从左引号开始。非引号为 no-op。
func (c *PunctuationConverter) RevertLastQuote(r rune) {
	switch r {
	case '\'':
		c.singleQuoteLeft = !c.singleQuoteLeft
	case '"':
		c.doubleQuoteLeft = !c.doubleQuoteLeft
	}
}

// ToEnglishPunct converts Chinese punctuation to English punctuation
// 中文标点转英文标点
func ToEnglishPunct(r rune) (rune, bool) {
	if english, ok := chineseToEnglishPunct[r]; ok {
		return english, true
	}
	return r, false
}

// ToChinesePunctString converts all English punctuation in a string to Chinese
func (c *PunctuationConverter) ToChinesePunctString(s string) string {
	runes := []rune(s)
	result := make([]rune, 0, len(runes))

	for _, r := range runes {
		if converted, ok := c.ToChinesePunct(r); ok {
			result = append(result, converted)
		} else {
			result = append(result, r)
		}
	}

	return string(result)
}

// ToEnglishPunctString converts all Chinese punctuation in a string to English
func ToEnglishPunctString(s string) string {
	runes := []rune(s)
	result := make([]rune, 0, len(runes))

	for _, r := range runes {
		if converted, ok := ToEnglishPunct(r); ok {
			result = append(result, converted)
		} else {
			result = append(result, r)
		}
	}

	return string(result)
}

// IsChinesePunct checks if a rune is a Chinese punctuation mark
func IsChinesePunct(r rune) bool {
	_, ok := chineseToEnglishPunct[r]
	return ok
}

// IsEnglishPunct checks if a rune is an English punctuation mark that can be converted
func IsEnglishPunct(r rune) bool {
	_, ok := englishToChinesePunct[r]
	return ok
}
