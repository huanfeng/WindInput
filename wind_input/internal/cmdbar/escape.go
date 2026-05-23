package cmdbar

import "strings"

// DecodeEscapes 宽松解码短语字面文本中的转义序列。
// 识别: \n→LF, \r→CR, \t→TAB, \\→\
// 未知 \X (含 Windows 路径反斜杠) 连同反斜杠原样保留, 不报错。
// 末尾孤立的 \ 原样保留。无反斜杠时走快路径原样返回。
//
// \ + ASCII 字母是保留命名空间, 用户不应依赖未知 \X 保持字面值;
// 详见 docs/design/command-bar-escape-support.md §2.3。
func DecodeEscapes(s string) string {
	if strings.IndexByte(s, '\\') < 0 {
		return s
	}
	var b strings.Builder
	b.Grow(len(s))
	for i := 0; i < len(s); i++ {
		c := s[i]
		if c != '\\' || i+1 >= len(s) {
			b.WriteByte(c)
			continue
		}
		switch s[i+1] {
		case 'n':
			b.WriteByte('\n')
		case 'r':
			b.WriteByte('\r')
		case 't':
			b.WriteByte('\t')
		case '\\':
			b.WriteByte('\\')
		default:
			// 未知转义: 原样保留反斜杠与后续字符。
			b.WriteByte('\\')
			b.WriteByte(s[i+1])
		}
		i++
	}
	return b.String()
}
