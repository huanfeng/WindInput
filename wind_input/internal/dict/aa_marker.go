package dict

import (
	"strconv"
	"strings"
)

// ParseAAMarker 解析 $AA("name", "chars") 字符组 marker。
//
// 返回 (groupName, chars, true) 或 ok=false (不是合法 $AA 形式)。
// 严格要求: 顶层语法 `$AA(STRING, STRING)`, 两个参数都是双引号字符串字面量,
// 用 strconv.Unquote 处理转义。**不支持单引号**, 必须使用双引号。
//
// 设计意图: 短语 yaml 用 `text: '$AA("标点", "、。")'` 形式表达字符组,
// 取代旧的 `texts` + `name` 双字段, 让 yaml 入口统一只用 `text:`。
// 详见 docs/design/command-bar-design.md §3.7。
func ParseAAMarker(value string) (name, chars string, ok bool) {
	v := strings.TrimSpace(value)
	if !strings.HasPrefix(v, "$AA(") || !strings.HasSuffix(v, ")") {
		return "", "", false
	}
	body := v[len("$AA(") : len(v)-1]
	body = strings.TrimSpace(body)

	nameStr, rest, ok := readQuotedString(body)
	if !ok {
		return "", "", false
	}
	rest = strings.TrimSpace(rest)
	if !strings.HasPrefix(rest, ",") {
		return "", "", false
	}
	rest = strings.TrimSpace(rest[1:])

	charsStr, rest, ok := readQuotedString(rest)
	if !ok {
		return "", "", false
	}
	if strings.TrimSpace(rest) != "" {
		return "", "", false // 多余内容
	}
	return nameStr, charsStr, true
}

// HasAAMarker 判断字符串是否以 $AA( 开头 (粗略检查, 不验证完整性)。
// 用于快速旁路: 含 $AA 的字符串走字符组路径, 不再当成普通模板/cmdbar 解析。
func HasAAMarker(value string) bool {
	return strings.HasPrefix(strings.TrimSpace(value), "$AA(")
}

// readQuotedString 从 s 开头读一个 Go 风格双引号字符串字面量,
// 用 strconv.Unquote 解码; 返回 (解码后, 字符串结束后的剩余 s, ok)。
func readQuotedString(s string) (decoded, rest string, ok bool) {
	if len(s) == 0 || s[0] != '"' {
		return "", "", false
	}
	end := 1
	for end < len(s) {
		if s[end] == '\\' {
			end += 2
			continue
		}
		if s[end] == '"' {
			literal := s[:end+1]
			d, err := strconv.Unquote(literal)
			if err != nil {
				return "", "", false
			}
			return d, s[end+1:], true
		}
		end++
	}
	return "", "", false
}
