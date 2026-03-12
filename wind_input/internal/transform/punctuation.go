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
}

// NewPunctuationConverter creates a new punctuation converter
func NewPunctuationConverter() *PunctuationConverter {
	return &PunctuationConverter{
		singleQuoteLeft: true,
		doubleQuoteLeft: true,
	}
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
		if c.singleQuoteLeft {
			c.singleQuoteLeft = false
			return chineseSingleQuoteLeft, true
		} else {
			c.singleQuoteLeft = true
			return chineseSingleQuoteRight, true
		}
	case '"':
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

// GetChinesePunctuation returns the Chinese punctuation for an English one
// without state tracking (for simple lookups)
func GetChinesePunctuation(r rune) (rune, bool) {
	if chinese, ok := englishToChinesePunct[r]; ok {
		return chinese, true
	}
	return r, false
}
