package dict

import (
	"fmt"
	"strings"
	"time"

	"github.com/google/uuid"
)

// TemplateEngine 变量模板引擎
// 支持 $var 和 ${var} 两种语法，将文本中的变量替换为实际值。
// 变量名区分大小写：$Y=四位年, $y=两位年, $MM=补零月, $M=不补零月 等。
type TemplateEngine struct {
	// 变量定义注册表
	variables map[string]VariableFunc
	// 按名称长度倒序排列的变量名（用于贪心匹配）
	sortedNames []string
}

// VariableFunc 变量求值函数
type VariableFunc func() string

// NewTemplateEngine 创建模板引擎并注册所有内置变量
func NewTemplateEngine() *TemplateEngine {
	te := &TemplateEngine{
		variables: make(map[string]VariableFunc),
	}
	te.registerBuiltinVariables()
	return te
}

// registerBuiltinVariables 注册所有内置变量
func (te *TemplateEngine) registerBuiltinVariables() {
	now := func() time.Time { return time.Now() }

	// ===== 日期 =====
	te.Register("YYYY", func() string { return now().Format("2006") })          // 四位年
	te.Register("Y", func() string { return now().Format("2006") })             // 四位年（别名）
	te.Register("YY", func() string { return now().Format("06") })              // 两位年
	te.Register("y", func() string { return now().Format("06") })               // 两位年（别名）
	te.Register("MM", func() string { return now().Format("01") })              // 月（补零）
	te.Register("M", func() string { return fmt.Sprintf("%d", now().Month()) }) // 月（不补零）
	te.Register("DD", func() string { return now().Format("02") })              // 日（补零）
	te.Register("D", func() string { return fmt.Sprintf("%d", now().Day()) })   // 日（不补零）

	// ===== 时间 =====
	te.Register("HH", func() string { return now().Format("15") })               // 时24h（补零）
	te.Register("H", func() string { return fmt.Sprintf("%d", now().Hour()) })   // 时24h（不补零）
	te.Register("h", func() string { return fmt.Sprintf("%d", now().Hour()) })   // 时（别名）
	te.Register("mm", func() string { return now().Format("04") })               // 分（补零）
	te.Register("mi", func() string { return now().Format("04") })               // 分（别名，兼容极点）
	te.Register("m", func() string { return fmt.Sprintf("%d", now().Minute()) }) // 分（不补零）
	te.Register("ss", func() string { return now().Format("05") })               // 秒（补零）
	te.Register("s", func() string { return fmt.Sprintf("%d", now().Second()) }) // 秒（不补零）

	// ===== 星期 =====
	te.Register("W", func() string {
		weekdays := []string{"星期日", "星期一", "星期二", "星期三", "星期四", "星期五", "星期六"}
		return weekdays[now().Weekday()]
	})
	te.Register("WC", func() string {
		weekdays := []string{"日", "一", "二", "三", "四", "五", "六"}
		return weekdays[now().Weekday()]
	})
	te.Register("w", func() string { return fmt.Sprintf("%d", now().Weekday()) }) // 星期数字(0=日)

	// ===== 中文 =====
	te.Register("YC", func() string { return toChinese(now().Format("2006")) })
	te.Register("MC", func() string { return toChineseNum(int(now().Month())) })
	te.Register("DC", func() string { return toChineseNum(now().Day()) })

	// ===== 特殊 =====
	te.Register("uuid", func() string { return uuid.New().String() })
	te.Register("ts", func() string { return fmt.Sprintf("%d", now().Unix()) })
	te.Register("tsms", func() string { return fmt.Sprintf("%d", now().UnixMilli()) })

	// 构建按名称长度倒序排列的列表（用于贪心匹配）
	te.buildSortedNames()
}

// Register 注册自定义变量
func (te *TemplateEngine) Register(name string, fn VariableFunc) {
	te.variables[name] = fn
}

// buildSortedNames 构建按长度倒序排列的变量名列表
func (te *TemplateEngine) buildSortedNames() {
	te.sortedNames = make([]string, 0, len(te.variables))
	for name := range te.variables {
		te.sortedNames = append(te.sortedNames, name)
	}
	// 按长度倒序，确保长名称优先匹配（如 $YYYY 优先于 $Y）
	for i := 0; i < len(te.sortedNames); i++ {
		for j := i + 1; j < len(te.sortedNames); j++ {
			if len(te.sortedNames[j]) > len(te.sortedNames[i]) {
				te.sortedNames[i], te.sortedNames[j] = te.sortedNames[j], te.sortedNames[i]
			}
		}
	}
}

// Expand 展开模板中的变量
// 支持两种语法：
//   - $var     — 简单变量，贪心匹配最长变量名
//   - ${var}   — 花括号变量，精确匹配
//
// 示例：
//
//	"$Y年$MM月$DD日"       → "2026年03月26日"
//	"${Y}-${MM}-${DD}"    → "2026-03-26"
//	"当前时间$HH:$mm:$ss"  → "当前时间14:30:05"
func (te *TemplateEngine) Expand(text string) string {
	if !strings.Contains(text, "$") {
		return text
	}

	var result strings.Builder
	result.Grow(len(text) * 2)

	i := 0
	runes := []rune(text)
	n := len(runes)

	for i < n {
		if runes[i] != '$' {
			result.WriteRune(runes[i])
			i++
			continue
		}

		// 找到 $ 符号
		if i+1 >= n {
			// $ 在末尾，原样输出
			result.WriteRune('$')
			i++
			continue
		}

		// 转义：$$ → $
		if runes[i+1] == '$' {
			result.WriteRune('$')
			i += 2
			continue
		}

		// ${var} 语法
		if runes[i+1] == '{' {
			endIdx := -1
			for j := i + 2; j < n; j++ {
				if runes[j] == '}' {
					endIdx = j
					break
				}
			}
			if endIdx == -1 {
				// 没找到 }，原样输出
				result.WriteRune('$')
				i++
				continue
			}
			varName := string(runes[i+2 : endIdx])
			if fn, ok := te.variables[varName]; ok {
				result.WriteString(fn())
			} else {
				// 未知变量，原样输出
				result.WriteString(string(runes[i : endIdx+1]))
			}
			i = endIdx + 1
			continue
		}

		// $var 语法 — 贪心匹配最长变量名
		matched := false
		remaining := string(runes[i+1:])
		for _, name := range te.sortedNames {
			if strings.HasPrefix(remaining, name) {
				if fn, ok := te.variables[name]; ok {
					result.WriteString(fn())
					i += 1 + len([]rune(name))
					matched = true
					break
				}
			}
		}
		if !matched {
			// 没有匹配的变量名，原样输出 $
			result.WriteRune('$')
			i++
		}
	}

	return result.String()
}

// HasVariable 检测文本是否包含变量引用
func HasVariable(text string) bool {
	return strings.Contains(text, "$")
}

// ===== 辅助函数 =====

// toChinese 将数字字符串逐字转换为中文（如 "2026" → "二〇二六"）
func toChinese(s string) string {
	digits := map[rune]string{
		'0': "〇", '1': "一", '2': "二", '3': "三", '4': "四",
		'5': "五", '6': "六", '7': "七", '8': "八", '9': "九",
	}
	var result strings.Builder
	for _, r := range s {
		if ch, ok := digits[r]; ok {
			result.WriteString(ch)
		} else {
			result.WriteRune(r)
		}
	}
	return result.String()
}

// toChineseNum 将数字转换为中文数字（如 26 → "二十六"）
func toChineseNum(n int) string {
	if n <= 0 {
		return "零"
	}
	if n <= 10 {
		units := []string{"零", "一", "二", "三", "四", "五", "六", "七", "八", "九", "十"}
		return units[n]
	}
	if n < 20 {
		units := []string{"零", "一", "二", "三", "四", "五", "六", "七", "八", "九"}
		return "十" + units[n-10]
	}
	if n < 100 {
		tens := n / 10
		ones := n % 10
		units := []string{"零", "一", "二", "三", "四", "五", "六", "七", "八", "九"}
		if ones == 0 {
			return units[tens] + "十"
		}
		return units[tens] + "十" + units[ones]
	}
	return fmt.Sprintf("%d", n)
}

// 全局模板引擎实例
var globalTemplateEngine *TemplateEngine

// GetTemplateEngine 获取全局模板引擎实例
func GetTemplateEngine() *TemplateEngine {
	if globalTemplateEngine == nil {
		globalTemplateEngine = NewTemplateEngine()
	}
	return globalTemplateEngine
}

// ExpandTemplate 使用全局引擎展开模板（便捷函数）
func ExpandTemplate(text string) string {
	return GetTemplateEngine().Expand(text)
}
