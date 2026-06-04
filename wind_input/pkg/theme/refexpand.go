package theme

import (
	"fmt"
	"image/color"
	"regexp"
	"strings"
)

// refRe 匹配 ${name} 引用，name 由字母数字下划线构成。
var refRe = regexp.MustCompile(`^\$\{([a-zA-Z_][a-zA-Z0-9_]*)\}$`)

// resolveColorTokens 把 v3 colors token 表在 isDark 环境下逐 token 递归求值为终值颜色。
//
// 求值语义（见 docs/design/theme-schema-v3.md「解析管线」第 3 步）：
//   - isDark 是贯穿求值的环境参数（非最后一步）：LightDark.Select(isDark) 先选分支；
//   - 选出的标量若是 "${token}" → 查 colors 表替换，对结果继续求值（多跳展开 + 循环保护）；
//   - 直到 hex/transparent 标量，再 ParseColor。primary 作为隐式 token 参与引用解析。
//
// 返回每个 token 的终值 color.Color；引用未知 token 或成环 → 报错（fail fast）。
func resolveColorTokens(tokens map[string]Color, primary string, isDark bool) (map[string]color.Color, error) {
	if hasRef(primary) {
		return nil, fmt.Errorf("palette.primary 不允许使用 ${} 引用: %q", primary)
	}
	out := make(map[string]color.Color, len(tokens)+1)

	// resolveScalar 把一个标量（hex / transparent / ${token}）在当前环境下展开到终值字符串。
	var resolveScalar func(s string, seen map[string]bool) (string, error)
	resolveScalar = func(s string, seen map[string]bool) (string, error) {
		if !hasRef(s) {
			return s, nil
		}
		mm := refRe.FindStringSubmatch(strings.TrimSpace(s))
		if mm == nil {
			return "", fmt.Errorf("不支持的颜色引用形态（仅支持单一 ${token}）: %q", s)
		}
		name := mm[1]
		if name == "primary" {
			return primary, nil
		}
		if seen[name] {
			return "", fmt.Errorf("颜色 token 引用成环: %q", name)
		}
		seen[name] = true
		ref, ok := tokens[name]
		if !ok {
			return "", fmt.Errorf("颜色引用未知 token: ${%s}", name)
		}
		return resolveScalar(ref.Select(isDark), seen)
	}

	for name, c := range tokens {
		scalar, err := resolveScalar(c.Select(isDark), map[string]bool{name: true})
		if err != nil {
			return nil, fmt.Errorf("token %q: %w", name, err)
		}
		out[name] = parseColorOrTransparent(scalar)
	}
	out["primary"] = parseColorOrTransparent(primary)
	return out, nil
}

func hasRef(s string) bool {
	return strings.Contains(s, "${")
}
