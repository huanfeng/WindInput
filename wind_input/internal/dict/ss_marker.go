package dict

import "strings"

// HasSSMarker 判断字符串是否以 $SS( 开头 (粗略检查, 不验证完整性)。
// 用于快速旁路: $SS 短语走字符串数组路径 (含嵌套 $CC 元素), 而非普通 dynamic。
func HasSSMarker(value string) bool {
	return strings.HasPrefix(strings.TrimSpace(value), "$SS(")
}

// ParseSSGroupName 从 $SS("name", elem1, elem2, ...) 静态提取第一个参数
// (group display name)。仅做最小扫描 —— 第一个参数必须是 Go 风格双引号
// string literal, 不允许 interpolation。失败返回 ok=false (例如格式错误,
// 或第一参不是 string lit)。
//
// 该函数用于 LoadFromStore 阶段把 group name 注入 phraseGroups, 避免在
// 词库加载时依赖 cmdbar parser/eval 求值。元素 (含嵌入 $CC) 仍在运行时
// 通过 CmdbarArrayHook 解析展开。
//
// 设计 docs/design/command-bar-followup.md §4.3。
func ParseSSGroupName(value string) (name string, ok bool) {
	v := strings.TrimSpace(value)
	if !strings.HasPrefix(v, "$SS(") || !strings.HasSuffix(v, ")") {
		return "", false
	}
	body := strings.TrimSpace(v[len("$SS("):])
	nameStr, _, ok := readQuotedString(body)
	if !ok {
		return "", false
	}
	return nameStr, true
}
