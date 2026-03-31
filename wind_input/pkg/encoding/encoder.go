package encoding

import (
	"fmt"
)

// Rule 编码规则
type Rule struct {
	LengthEqual int    // 精确匹配词长（0 表示不使用）
	LengthRange [2]int // 范围匹配 [min, max]（[0,0] 表示不使用）
	Formula     string // 编码公式，如 "AaAbBaBb"
}

// FormulaStep 公式中的一步
type FormulaStep struct {
	CharIndex int // 字序：0-based，-1 表示末字（Z）
	CodeIndex int // 码序：0-based（a=0, b=1, c=2, ...）
}

// ParseFormula 解析编码公式为步骤列表
// formula 必须偶数长度，每 2 个字符一组：
// 大写字母(A-Z)=字序(A=0,B=1,...,Z=-1表示末字)，小写字母(a-z)=码序(a=0,b=1,...)
func ParseFormula(formula string) ([]FormulaStep, error) {
	if len(formula)%2 != 0 {
		return nil, fmt.Errorf("formula length must be even, got %d", len(formula))
	}
	steps := make([]FormulaStep, 0, len(formula)/2)
	for i := 0; i < len(formula); i += 2 {
		upper := formula[i]
		lower := formula[i+1]
		if upper < 'A' || upper > 'Z' {
			return nil, fmt.Errorf("expected uppercase letter at position %d, got '%c'", i, upper)
		}
		if lower < 'a' || lower > 'z' {
			return nil, fmt.Errorf("expected lowercase letter at position %d, got '%c'", i+1, lower)
		}
		var charIndex int
		if upper == 'Z' {
			charIndex = -1
		} else {
			charIndex = int(upper - 'A')
		}
		codeIndex := int(lower - 'a')
		steps = append(steps, FormulaStep{CharIndex: charIndex, CodeIndex: codeIndex})
	}
	return steps, nil
}

// MatchRule 从规则列表中找到匹配给定词长的规则
// 先检查 LengthEqual 精确匹配，再检查 LengthRange 范围匹配
func MatchRule(rules []Rule, wordLen int) *Rule {
	for i := range rules {
		if rules[i].LengthEqual != 0 && rules[i].LengthEqual == wordLen {
			return &rules[i]
		}
	}
	for i := range rules {
		lr := rules[i].LengthRange
		if lr[0] != 0 || lr[1] != 0 {
			if wordLen >= lr[0] && wordLen <= lr[1] {
				return &rules[i]
			}
		}
	}
	return nil
}

// CalcWordCode 根据规则计算词的编码
// word 必须 >= 2 个 rune
// charCodes 为每个汉字对应的全码
func CalcWordCode(word string, charCodes map[string]string, rules []Rule) (string, error) {
	runes := []rune(word)
	if len(runes) < 2 {
		return "", fmt.Errorf("word must have at least 2 characters, got %d", len(runes))
	}

	rule := MatchRule(rules, len(runes))
	if rule == nil {
		return "", fmt.Errorf("no matching rule for word length %d", len(runes))
	}

	steps, err := ParseFormula(rule.Formula)
	if err != nil {
		return "", fmt.Errorf("invalid formula %q: %w", rule.Formula, err)
	}

	result := make([]byte, 0, len(steps))
	for _, step := range steps {
		var ch rune
		if step.CharIndex == -1 {
			ch = runes[len(runes)-1]
		} else {
			if step.CharIndex >= len(runes) {
				return "", fmt.Errorf("char index %d out of range for word length %d", step.CharIndex, len(runes))
			}
			ch = runes[step.CharIndex]
		}

		charStr := string(ch)
		code, ok := charCodes[charStr]
		if !ok {
			return "", fmt.Errorf("no code found for character %q", charStr)
		}

		if step.CodeIndex >= len(code) {
			return "", fmt.Errorf("code index %d out of range for character %q (code=%q, len=%d)", step.CodeIndex, charStr, code, len(code))
		}

		result = append(result, code[step.CodeIndex])
	}

	return string(result), nil
}
