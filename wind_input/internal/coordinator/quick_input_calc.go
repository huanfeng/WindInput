// quick_input_calc.go — 快捷输入：数学计算器模块
package coordinator

import (
	"errors"
	"fmt"
	"math"
	"strconv"
	"strings"
)

// isCalcExpression 检查字符串是否为计算表达式（包含运算符，以数字或左括号开头）
func isCalcExpression(s string) bool {
	if len(s) == 0 {
		return false
	}
	if s[0] != '(' && (s[0] < '0' || s[0] > '9') {
		return false
	}
	for _, r := range s {
		switch r {
		case '+', '-', '*', '/':
			return true
		case '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', '.', '(', ')':
			continue
		default:
			return false
		}
	}
	return false
}

// evaluateExpression 计算数学表达式（支持 +, -, *, / 和运算符优先级）
// 使用递归下降解析器实现，正确处理乘除优先于加减。
func evaluateExpression(expr string) (float64, error) {
	p := &exprParser{input: expr}
	result, err := p.parseExpr()
	if err != nil {
		return 0, err
	}
	return result, nil
}

type exprParser struct {
	input string
	pos   int
}

func (p *exprParser) parseExpr() (float64, error) {
	left, err := p.parseTerm()
	if err != nil {
		return 0, err
	}
	for p.pos < len(p.input) {
		op := p.input[p.pos]
		if op != '+' && op != '-' {
			break
		}
		p.pos++
		right, err := p.parseTerm()
		if err != nil {
			return 0, err
		}
		if op == '+' {
			left += right
		} else {
			left -= right
		}
	}
	return left, nil
}

func (p *exprParser) parseTerm() (float64, error) {
	left, err := p.parsePrimary()
	if err != nil {
		return 0, err
	}
	for p.pos < len(p.input) {
		op := p.input[p.pos]
		if op != '*' && op != '/' {
			break
		}
		p.pos++
		right, err := p.parsePrimary()
		if err != nil {
			return 0, err
		}
		if op == '*' {
			left *= right
		} else {
			if right == 0 {
				return 0, errors.New("division by zero")
			}
			left /= right
		}
	}
	return left, nil
}

// parsePrimary 解析基本单元：括号表达式或数字
func (p *exprParser) parsePrimary() (float64, error) {
	if p.pos < len(p.input) && p.input[p.pos] == '(' {
		p.pos++ // 消费 '('
		val, err := p.parseExpr()
		if err != nil {
			return 0, err
		}
		if p.pos >= len(p.input) || p.input[p.pos] != ')' {
			return 0, fmt.Errorf("expected ')' at position %d", p.pos)
		}
		p.pos++ // 消费 ')'
		return val, nil
	}
	return p.parseNumber()
}

func (p *exprParser) parseNumber() (float64, error) {
	start := p.pos
	for p.pos < len(p.input) {
		c := p.input[p.pos]
		if (c >= '0' && c <= '9') || c == '.' {
			p.pos++
		} else {
			break
		}
	}
	if start == p.pos {
		return 0, fmt.Errorf("expected number at position %d", p.pos)
	}
	return strconv.ParseFloat(p.input[start:p.pos], 64)
}

// formatCalcResultPrec 将浮点结果格式化为字符串，支持精度控制
// decimalPlaces <= 0 表示四舍五入为整数，否则最多保留指定位数并去除尾部零。
func formatCalcResultPrec(val float64, decimalPlaces int) string {
	if math.IsNaN(val) || math.IsInf(val, 0) {
		return strconv.FormatFloat(val, 'f', -1, 64)
	}
	if decimalPlaces <= 0 {
		// 0 或负数：四舍五入为整数
		rounded := math.Round(val)
		if rounded >= math.MinInt64 && rounded <= math.MaxInt64 {
			return strconv.FormatInt(int64(rounded), 10)
		}
		return strconv.FormatFloat(rounded, 'f', 0, 64)
	}
	// 整数结果直接输出（无小数点）
	if val == math.Trunc(val) && val >= math.MinInt64 && val <= math.MaxInt64 {
		return strconv.FormatInt(int64(val), 10)
	}
	s := strconv.FormatFloat(val, 'f', decimalPlaces, 64)
	// 去除尾部零
	if strings.Contains(s, ".") {
		s = strings.TrimRight(s, "0")
		s = strings.TrimRight(s, ".")
	}
	return s
}

// formatCalcResult 将浮点结果格式化为字符串（默认保留 6 位小数）
func formatCalcResult(val float64) string {
	return formatCalcResultPrec(val, 6)
}

// hasOperator 检查字符串是否包含运算符
func hasOperator(s string) bool {
	for _, r := range s {
		if r == '+' || r == '-' || r == '*' || r == '/' {
			return true
		}
	}
	return false
}

// generateCalcCandidates 根据计算表达式生成候选列表
func generateCalcCandidates(expr string, decimalPlaces int) []string {
	cleanExpr := strings.TrimRight(expr, "+-*/")
	if cleanExpr == "" {
		return nil
	}

	// 去掉尾部运算符后，如果不包含运算符则不计算
	if !hasOperator(cleanExpr) {
		return nil
	}

	val, err := evaluateExpression(cleanExpr)
	if err != nil {
		return nil
	}

	resultStr := formatCalcResultPrec(val, decimalPlaces)
	candidates := make([]string, 0, 8)
	candidates = append(candidates, cleanExpr+"="+resultStr)
	candidates = append(candidates, resultStr)

	if val == math.Trunc(val) && val >= 0 && !math.IsInf(val, 0) {
		intStr := strconv.FormatInt(int64(val), 10)
		candidates = append(candidates, numberToAmount(intStr, true))
		candidates = append(candidates, numberToAmount(intStr, false))
		candidates = append(candidates, numberToChineseLower(intStr))
		candidates = append(candidates, numberToChineseUpper(intStr))
		candidates = append(candidates, digitsToChineseChars(intStr, false))
		candidates = append(candidates, digitsToChineseChars(intStr, true))
	} else if !math.IsInf(val, 0) && !math.IsNaN(val) {
		// 小数结果：生成中文读法
		intPart, decPart := splitDecimal(resultStr)
		if decPart != "" {
			candidates = append(candidates, decimalToChineseText(intPart, decPart, false))
			candidates = append(candidates, decimalToChineseText(intPart, decPart, true))
		}
	}

	return candidates
}
