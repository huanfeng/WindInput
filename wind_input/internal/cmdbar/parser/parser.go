package parser

import (
	"errors"
	"fmt"
	"strings"

	"github.com/huanfeng/wind_input/internal/cmdbar/ast"
)

// Parse parses a phrase source string into an ast.Phrase. The top-level
// classification rule is:
//
//  1. If the source contains a marker token (`$CC(` / `$CC1(` / `$SS(`)
//     at depth 0 (and not inside a string), dispatch:
//     - `$CC(` / `$CC1(` → CommandPhrase ($CC1 = $CC + {prefix:true} sugar)
//     - `$SS(`           → ArrayPhrase (字符串数组, 元素可嵌入 $CC)
//  2. Otherwise if the source contains an unescaped `{` at top level,
//     treat the whole source as a TemplatePhrase with an implicit
//     surrounding string literal.
//  3. Otherwise produce a LiteralPhrase carrying the raw text.
//
// Errors include the offending byte offset.
func Parse(src string) (ast.Phrase, error) {
	marker, idx, openOff := findTopLevelMarker(src)
	if idx >= 0 {
		// Slice before the marker is treated as a literal prefix; we
		// require it to be empty or whitespace-only per the design (a
		// phrase is either literal / template / command — they don't
		// concatenate at top level).
		prefix := src[:idx]
		if strings.TrimSpace(prefix) != "" {
			return nil, fmt.Errorf("unexpected text before %s at offset 0", marker)
		}
		switch marker {
		case "$CC", "$CC1":
			return parseCommandPhrase(src, idx, openOff)
		case "$SS":
			return parseArrayPhrase(src, idx, openOff)
		}
	}
	if hasTopLevelBrace(src) {
		return parseTemplatePhrase(src)
	}
	return ast.LiteralPhrase{Text: src}, nil
}

// markerTable lists recognized top-level markers, longest-prefix-first.
// 顺序敏感: `$CC1` 必须排在 `$CC` 之前, 否则 `$CC1(` 会被吃成 `$CC(` + 残留 "1(".
var markerTable = []string{"$CC1", "$CC", "$SS"}

// findTopLevelMarker scans src for the first marker token outside any
// string literal. 返回 (markerName, idx, openParenOff). 未找到时
// (markerName="", idx=-1, openParenOff=-1)。
//
// idx 指向 '$' 字节, openParenOff 指向紧邻的 '(' 字节。
func findTopLevelMarker(src string) (string, int, int) {
	i := 0
	for i < len(src) {
		c := src[i]
		if c == '\\' && i+1 < len(src) {
			i += 2
			continue
		}
		if c == '"' || c == '\'' {
			// skip string
			q := c
			i++
			for i < len(src) {
				if src[i] == '\\' && i+1 < len(src) {
					i += 2
					continue
				}
				if src[i] == q {
					i++
					break
				}
				i++
			}
			continue
		}
		if c == '$' {
			for _, m := range markerTable {
				end := i + len(m)
				if end < len(src) && src[end] == '(' && src[i:end] == m {
					return m, i, end
				}
			}
		}
		i++
	}
	return "", -1, -1
}

// hasTopLevelBrace reports whether src contains an unescaped `{` that is
// not inside a string literal.
func hasTopLevelBrace(src string) bool {
	i := 0
	for i < len(src) {
		c := src[i]
		if c == '\\' && i+1 < len(src) {
			i += 2
			continue
		}
		if c == '{' {
			return true
		}
		i++
	}
	return false
}

func parseCommandPhrase(src string, idx, open int) (ast.Phrase, error) {
	// idx points at '$'; `open` 指向 '(' (对 `$CC(` 是 idx+3, 对 `$CC1(` 是 idx+4)。
	// Build a sub-source representing "$CC(...)" / "$CC1(...)" and tokenize the
	// inner expression list. We reuse the expression lexer by scanning from
	// open+1 until the matching ')'.
	if open >= len(src) || src[open] != '(' {
		return nil, fmt.Errorf("expected '(' after $CC at offset %d", idx)
	}
	// Marker name 区间是 src[idx : open] (含 '$', 不含 '(' ), 例如 "$CC" / "$CC1"。
	markerName := src[idx:open]
	// Find matching ')' at depth 0 ignoring strings.
	end, err := findMatchingParen(src, open)
	if err != nil {
		return nil, err
	}
	inner := src[open+1 : end]
	// Trailing characters after the closing paren must be empty/whitespace.
	if strings.TrimSpace(src[end+1:]) != "" {
		return nil, fmt.Errorf("unexpected text after %s(...) at offset %d", markerName, end+1)
	}
	lex := NewLexer(inner)
	toks, err := lex.Tokenize()
	if err != nil {
		return nil, fmt.Errorf("%s body: %w", markerName, err)
	}
	p := &parser{tokens: toks, src: inner, baseOff: open + 1}
	args, err := p.parseExprList()
	if err != nil {
		return nil, err
	}
	if p.peek().Kind != tkEOF {
		return nil, p.errf(p.peek().Offset, "unexpected token %q", p.peek().Lexeme)
	}

	// Trailing options bag handling: if the last arg is an ObjectLit,
	// extract it as Modifiers; mid-list ObjectLits are rejected.
	var explicit map[string]any
	if n := len(args); n > 0 {
		if obj, ok := args[n-1].(ast.ObjectLit); ok {
			m, err := objectLitToMap(obj)
			if err != nil {
				return nil, fmt.Errorf("%s options: %w", markerName, err)
			}
			explicit = m
			args = args[:n-1]
		}
	}
	for i, a := range args {
		if _, ok := a.(ast.ObjectLit); ok {
			return nil, fmt.Errorf("%s: options bag must be the last argument (found at arg %d)", markerName, i+1)
		}
	}

	if len(args) == 0 {
		return nil, fmt.Errorf("%s requires a display expression at offset %d", markerName, idx)
	}

	// Marker syntax sugar: apply marker-specific defaults, then let
	// explicit options override. Centralized in markerDefaults so adding
	// new sugar (e.g. future `$SS1`) only needs a table entry.
	modifiers := mergeModifiers(markerDefaults(markerName), explicit)
	return ast.CommandPhrase{Display: args[0], Actions: args[1:], Modifiers: modifiers}, nil
}

// markerDefaults returns the default modifier values implicit in a marker
// name. 表对应 docs/design/command-bar-followup.md §4.2 marker 登记表:
//   - `$CC`  → 无默认 (prefix=false 是隐式默认, 在 dict 层兜底)
//   - `$CC1` → {prefix: true} (`$CC + prefix:true` 简写)
//   - `$SS`  → {prefix: true, expand: "exact", nav: true} (字符串数组默认前缀展开)
//
// 未来 sugar markers ($SS1 等) 在此处加表项, 不需要触动 parser 核心逻辑。
func markerDefaults(markerName string) map[string]any {
	switch markerName {
	case "$CC1":
		return map[string]any{"prefix": true}
	case "$SS":
		return map[string]any{"prefix": true, "expand": "exact", "nav": true}
	}
	return nil
}

// mergeModifiers returns a fresh map combining defaults and explicit
// modifiers; explicit keys override defaults. Returns nil when both
// inputs are empty so AST consumers can use `len(p.Modifiers) > 0` as
// the "any modifiers present?" check.
func mergeModifiers(defaults, explicit map[string]any) map[string]any {
	if len(defaults) == 0 && len(explicit) == 0 {
		return nil
	}
	out := make(map[string]any, len(defaults)+len(explicit))
	for k, v := range defaults {
		out[k] = v
	}
	for k, v := range explicit {
		out[k] = v
	}
	return out
}

// objectLitToMap projects an ObjectLit's literal pairs to a flat
// map[string]any with Go-native value types: string / float64 / bool.
// Ident values "true" / "false" are coerced to bool; other idents are
// preserved as the raw symbol string (host-defined enums).
//
// Duplicate keys: last write wins (consistent with ECMAScript / YAML).
func objectLitToMap(obj ast.ObjectLit) (map[string]any, error) {
	if len(obj.Pairs) == 0 {
		return nil, nil
	}
	out := make(map[string]any, len(obj.Pairs))
	for _, pair := range obj.Pairs {
		v, err := evalModifierLiteral(pair.Value)
		if err != nil {
			return nil, fmt.Errorf("key %q: %w", pair.Key, err)
		}
		out[pair.Key] = v
	}
	return out, nil
}

// evalModifierLiteral converts a literal-only ast.Expr to its Go value.
// StringLit must be a pure literal (no `{expr}` interpolation) since
// modifier values are statically known at parse time. Number → float64,
// Ident "true"/"false" → bool, other Ident → string.
func evalModifierLiteral(e ast.Expr) (any, error) {
	switch v := e.(type) {
	case ast.StringLit:
		var b strings.Builder
		for _, part := range v.Parts {
			switch p := part.(type) {
			case ast.LiteralPart:
				b.WriteString(p.Text)
			case ast.InterpPart:
				return nil, fmt.Errorf("interpolation %s not allowed in modifier value", p.Expr)
			}
		}
		return b.String(), nil
	case ast.NumberLit:
		return v.Value, nil
	case ast.Ident:
		switch v.Name {
		case "true":
			return true, nil
		case "false":
			return false, nil
		}
		return v.Name, nil
	}
	return nil, fmt.Errorf("unsupported modifier value type %T", e)
}

// parseArrayPhrase 解析 `$SS("name", elem1, elem2, ..., {options})`。
//
// 设计 (docs/design/command-bar-followup.md §4.3 / §4.4):
//   - 第一参数必须是无 interp 的 string lit, 作为 group display name
//   - 后续参数每个是 string lit 或嵌入 $CC(...) / $CC1(...) CommandPhrase
//   - 末尾可选 ObjectLit 作为 group-level Modifiers (与 marker syntax sugar 合并)
//   - 嵌套深度 1: 内层 CommandPhrase 不能再嵌入 $SS / $AA / $CC marker
//     (由 lexer 报"unexpected character '$'" 兜底; CommandPhrase 解析时自身
//     的 Actions 走 parseExpr, 不识别 marker 调用)
//   - 内层 CommandPhrase 不允许带 prefix modifier (group prefix 由外层 $SS 控制)
//
// 为了支持元素中嵌入 $CC(, parseArrayPhrase 不能直接 lex 整个 inner,
// 而是先用 splitArrayArgs 按顶层 ',' 切片成 argSpan 列表, 再对每个 span
// 调 parseArrayElement 分别 parse (有 $CC 前缀的走 parseCommandPhrase,
// 否则走通用 parseExpr 接受 string/object)。
func parseArrayPhrase(src string, idx, open int) (ast.Phrase, error) {
	markerName := src[idx:open]
	if open >= len(src) || src[open] != '(' {
		return nil, fmt.Errorf("expected '(' after %s at offset %d", markerName, idx)
	}
	end, err := findMatchingParen(src, open)
	if err != nil {
		return nil, err
	}
	inner := src[open+1 : end]
	if strings.TrimSpace(src[end+1:]) != "" {
		return nil, fmt.Errorf("unexpected text after %s(...) at offset %d", markerName, end+1)
	}
	spans, err := splitArrayArgs(inner, open+1)
	if err != nil {
		return nil, fmt.Errorf("%s body: %w", markerName, err)
	}
	parsedArgs := make([]ast.Expr, 0, len(spans))
	for _, sp := range spans {
		e, err := parseArrayElement(sp.text, sp.offset)
		if err != nil {
			return nil, fmt.Errorf("%s element: %w", markerName, err)
		}
		parsedArgs = append(parsedArgs, e)
	}

	// Trailing options bag handling (same logic as parseCommandPhrase).
	var explicit map[string]any
	if n := len(parsedArgs); n > 0 {
		if obj, ok := parsedArgs[n-1].(ast.ObjectLit); ok {
			m, err := objectLitToMap(obj)
			if err != nil {
				return nil, fmt.Errorf("%s options: %w", markerName, err)
			}
			explicit = m
			parsedArgs = parsedArgs[:n-1]
		}
	}
	for i, a := range parsedArgs {
		if _, ok := a.(ast.ObjectLit); ok {
			return nil, fmt.Errorf("%s: options bag must be the last argument (found at arg %d)", markerName, i+1)
		}
	}

	if len(parsedArgs) < 1 {
		return nil, fmt.Errorf("%s requires a group name (first argument)", markerName)
	}
	nameLit, ok := parsedArgs[0].(ast.StringLit)
	if !ok {
		return nil, fmt.Errorf("%s: first argument must be a string literal (group name), got %T", markerName, parsedArgs[0])
	}
	name, err := stringLitToPlain(nameLit)
	if err != nil {
		return nil, fmt.Errorf("%s name: %w", markerName, err)
	}

	elements := parsedArgs[1:]
	for i, e := range elements {
		switch v := e.(type) {
		case ast.StringLit:
			// OK: string lit element (含 interp 也允许, eval 时再处理)
		case ast.CommandPhrase:
			// 嵌套 $CC 元素: 禁用 prefix modifier (group 控制)
			if _, hasPrefix := v.Modifiers["prefix"]; hasPrefix {
				return nil, fmt.Errorf("%s element %d: nested $CC must not set 'prefix' modifier (group prefix is controlled by %s options)", markerName, i+1, markerName)
			}
		default:
			return nil, fmt.Errorf("%s element %d: must be string literal or $CC(...), got %T", markerName, i+1, e)
		}
	}

	modifiers := mergeModifiers(markerDefaults(markerName), explicit)
	return ast.ArrayPhrase{Name: name, Elements: elements, Modifiers: modifiers}, nil
}

// argSpan represents one top-level argument slice from an $SS argument
// list. text 是 raw substring (含两侧空白), offset 是 text[0] 在原 phrase
// 源中的字节位置 (供 parser 错误信息基线使用)。
type argSpan struct {
	text   string
	offset int
}

// splitArrayArgs 按顶层逗号切割 inner, 跳过字符串字面量、括号/花括号嵌套。
// 空 inner 返回空 slice; 仅含空白也视为空。
//
// baseOff 是 inner[0] 在原 phrase 源中的字节位置, 用来计算每个 span 的绝对偏移。
func splitArrayArgs(inner string, baseOff int) ([]argSpan, error) {
	trimmed := strings.TrimSpace(inner)
	if trimmed == "" {
		return nil, nil
	}
	var out []argSpan
	depth := 0
	inString := byte(0)
	start := 0
	for i := 0; i < len(inner); i++ {
		c := inner[i]
		if inString != 0 {
			if c == '\\' && i+1 < len(inner) {
				i++ // skip escaped char (loop ++ then skips next byte)
				continue
			}
			if c == inString {
				inString = 0
			}
			continue
		}
		if c == '"' || c == '\'' {
			inString = c
			continue
		}
		if c == '\\' && i+1 < len(inner) {
			i++
			continue
		}
		switch c {
		case '(', '{', '[':
			depth++
		case ')', '}', ']':
			depth--
		case ',':
			if depth == 0 {
				out = append(out, argSpan{text: inner[start:i], offset: baseOff + start})
				start = i + 1
			}
		}
	}
	if inString != 0 {
		return nil, fmt.Errorf("unclosed string in array args at offset %d", baseOff)
	}
	if depth != 0 {
		return nil, fmt.Errorf("unbalanced brackets in array args at offset %d", baseOff)
	}
	out = append(out, argSpan{text: inner[start:], offset: baseOff + start})
	return out, nil
}

// parseArrayElement 解析一个 $SS 元素 span。判定路径:
//
//  1. trim 后若以 `$CC(` 或 `$CC1(` 开头, 走 parseCommandPhrase 子集 (产出
//     ast.CommandPhrase, 而 CommandPhrase 已实现 Expr 接口)
//  2. 否则 lex+parse 单个 expression (允许 StringLit 或 ObjectLit)
func parseArrayElement(text string, offset int) (ast.Expr, error) {
	leading := 0
	for leading < len(text) && (text[leading] == ' ' || text[leading] == '\t' || text[leading] == '\n' || text[leading] == '\r') {
		leading++
	}
	rest := text[leading:]
	if strings.HasPrefix(rest, "$CC1(") || strings.HasPrefix(rest, "$CC(") {
		marker, midx, mopen := findTopLevelMarker(text)
		if midx != leading || (marker != "$CC" && marker != "$CC1") {
			return nil, fmt.Errorf("element starts with $CC marker but parse failed at offset %d", offset)
		}
		// parseCommandPhrase 期待整个 text 是单个 $CC(...) 表达式,
		// 且 closing paren 之后只能是空白。我们的 span 已经按顶层 ','
		// 切过, 所以 closing paren 之后只可能是空白 (或 trailing
		// modifier bag 在 $SS 级别, 不会落到这里)。
		phrase, err := parseCommandPhrase(text, midx, mopen)
		if err != nil {
			return nil, err
		}
		cp, ok := phrase.(ast.CommandPhrase)
		if !ok {
			return nil, fmt.Errorf("internal: parseCommandPhrase returned %T", phrase)
		}
		return cp, nil
	}
	lex := NewLexer(text)
	toks, err := lex.Tokenize()
	if err != nil {
		return nil, err
	}
	p := &parser{tokens: toks, src: text, baseOff: offset}
	expr, err := p.parseExpr()
	if err != nil {
		return nil, err
	}
	if p.peek().Kind != tkEOF {
		return nil, p.errf(p.peek().Offset, "unexpected token %q in array element", p.peek().Lexeme)
	}
	return expr, nil
}

// stringLitToPlain returns the static text content of a StringLit, refusing
// any `{expr}` interpolation. Used for the $SS group name where the value
// must be statically known.
func stringLitToPlain(s ast.StringLit) (string, error) {
	var b strings.Builder
	for _, part := range s.Parts {
		switch v := part.(type) {
		case ast.LiteralPart:
			b.WriteString(v.Text)
		case ast.InterpPart:
			return "", fmt.Errorf("interpolation %s not allowed", v.Expr)
		}
	}
	return b.String(), nil
}

func findMatchingParen(src string, openIdx int) (int, error) {
	depth := 1
	i := openIdx + 1
	for i < len(src) {
		c := src[i]
		if c == '\\' && i+1 < len(src) {
			i += 2
			continue
		}
		if c == '"' || c == '\'' {
			q := c
			i++
			for i < len(src) {
				if src[i] == '\\' && i+1 < len(src) {
					i += 2
					continue
				}
				if src[i] == q {
					i++
					break
				}
				i++
			}
			continue
		}
		if c == '(' {
			depth++
		} else if c == ')' {
			depth--
			if depth == 0 {
				return i, nil
			}
		}
		i++
	}
	return -1, fmt.Errorf("unclosed '(' at offset %d", openIdx)
}

func parseTemplatePhrase(src string) (ast.Phrase, error) {
	// Treat the entire src as the body of an implicit double-quoted
	// string, sharing the same string-decoding rules: literal text and
	// `{expr}` interpolations. We hand-build a stringPart slice without
	// going through the lexer (since there are no outer quotes).
	var parts []stringPart
	var lit []byte
	flush := func(off int) {
		if len(lit) > 0 {
			parts = append(parts, stringPart{Lit: string(lit), Offset: off})
			lit = lit[:0]
		}
	}
	i := 0
	for i < len(src) {
		c := src[i]
		if c == '\\' && i+1 < len(src) {
			next := src[i+1]
			switch next {
			case '\\', '{', '}', '(', ')', '"', '\'':
				lit = append(lit, next)
			case 'n':
				lit = append(lit, '\n')
			case 't':
				lit = append(lit, '\t')
			case 'r':
				lit = append(lit, '\r')
			default:
				// 未知转义: 原样保留反斜杠与后续字符 (宽松白名单策略,
				// 与 cmdbar.DecodeEscapes 一致, 见
				// docs/design/command-bar-escape-support.md §2)。
				lit = append(lit, '\\', next)
			}
			i += 2
			continue
		}
		if c == '{' {
			flush(i)
			interpStart := i + 1
			depth := 1
			p := interpStart
			inInner := byte(0)
			for p < len(src) && depth > 0 {
				ch := src[p]
				if inInner != 0 {
					if ch == '\\' && p+1 < len(src) {
						p += 2
						continue
					}
					if ch == inInner {
						inInner = 0
					}
					p++
					continue
				}
				if ch == '"' || ch == '\'' {
					inInner = ch
					p++
					continue
				}
				if ch == '{' {
					depth++
				} else if ch == '}' {
					depth--
					if depth == 0 {
						break
					}
				}
				p++
			}
			if depth != 0 {
				return nil, fmt.Errorf("unclosed '{' at offset %d", i)
			}
			parts = append(parts, stringPart{IsInterp: true, Interp: src[interpStart:p], Offset: i})
			i = p + 1
			continue
		}
		if c == '}' {
			return nil, fmt.Errorf("unmatched '}' at offset %d", i)
		}
		lit = append(lit, c)
		i++
	}
	flush(len(src))
	str, err := buildStringLit(parts)
	if err != nil {
		return nil, err
	}
	return ast.TemplatePhrase{Expr: str}, nil
}

// buildStringLit converts a parts slice into an ast.StringLit, parsing
// each interpolation body as an expression.
func buildStringLit(parts []stringPart) (ast.StringLit, error) {
	out := make([]ast.Part, 0, len(parts))
	for _, p := range parts {
		if !p.IsInterp {
			out = append(out, ast.LiteralPart{Text: p.Lit})
			continue
		}
		body := p.Interp
		lex := NewLexer(body)
		toks, err := lex.Tokenize()
		if err != nil {
			return ast.StringLit{}, fmt.Errorf("interpolation at offset %d: %w", p.Offset, err)
		}
		pr := &parser{tokens: toks, src: body, baseOff: p.Offset + 1}
		expr, err := pr.parseExpr()
		if err != nil {
			return ast.StringLit{}, err
		}
		if pr.peek().Kind != tkEOF {
			return ast.StringLit{}, pr.errf(pr.peek().Offset, "unexpected token %q in interpolation", pr.peek().Lexeme)
		}
		out = append(out, ast.InterpPart{Expr: expr})
	}
	return ast.StringLit{Parts: out}, nil
}

// parser is a tiny recursive-descent parser over the token stream
// produced for an expression-level slice of source. baseOff is the byte
// offset of token offset 0 within the original phrase source, so error
// messages can map back to the user-visible position.
type parser struct {
	tokens  []Token
	pos     int
	src     string
	baseOff int
}

func (p *parser) peek() Token { return p.tokens[p.pos] }
func (p *parser) bump() Token {
	t := p.tokens[p.pos]
	p.pos++
	return t
}

func (p *parser) errf(off int, format string, args ...any) error {
	return fmt.Errorf("%s at offset %d", fmt.Sprintf(format, args...), p.baseOff+off)
}

// parseExprList parses zero or more comma-separated expressions.
func (p *parser) parseExprList() ([]ast.Expr, error) {
	if p.peek().Kind == tkEOF || p.peek().Kind == tkRParen {
		return nil, nil
	}
	var out []ast.Expr
	for {
		e, err := p.parseExpr()
		if err != nil {
			return nil, err
		}
		out = append(out, e)
		if p.peek().Kind != tkComma {
			break
		}
		p.bump() // ,
		if p.peek().Kind == tkEOF || p.peek().Kind == tkRParen {
			return nil, p.errf(p.peek().Offset, "trailing comma")
		}
	}
	return out, nil
}

func (p *parser) parseExpr() (ast.Expr, error) {
	t := p.peek()
	switch t.Kind {
	case tkNumber:
		p.bump()
		return ast.NumberLit{Value: t.Number, Raw: t.Lexeme}, nil
	case tkString:
		p.bump()
		return buildStringLit(t.Parts)
	case tkIdent:
		p.bump()
		name := t.Lexeme
		// Optional namespace: ident "." ident.
		if p.peek().Kind == tkDot {
			p.bump()
			if p.peek().Kind != tkIdent {
				return nil, p.errf(p.peek().Offset, "expected identifier after '.'")
			}
			second := p.bump()
			name = name + "." + second.Lexeme
			if p.peek().Kind == tkDot {
				return nil, p.errf(p.peek().Offset, "function name may have at most one '.'")
			}
		}
		// Call form?
		if p.peek().Kind == tkLParen {
			p.bump()
			var args []ast.Expr
			if p.peek().Kind != tkRParen {
				var err error
				args, err = p.parseExprList()
				if err != nil {
					return nil, err
				}
			}
			if p.peek().Kind != tkRParen {
				return nil, p.errf(p.peek().Offset, "expected ')' to close call to %s", name)
			}
			p.bump()
			return ast.Call{Name: name, Args: args}, nil
		}
		// Bare identifier. If a namespace was provided, demand parens.
		if strings.Contains(name, ".") {
			return nil, p.errf(t.Offset, "namespaced function %q must be called with ()", name)
		}
		return ast.Ident{Name: name}, nil
	case tkLParen:
		p.bump()
		e, err := p.parseExpr()
		if err != nil {
			return nil, err
		}
		if p.peek().Kind != tkRParen {
			return nil, p.errf(p.peek().Offset, "expected ')'")
		}
		p.bump()
		return e, nil
	case tkLBrace:
		return p.parseObjectLit()
	case tkEOF:
		return nil, p.errf(t.Offset, "unexpected end of input")
	}
	return nil, p.errf(t.Offset, "unexpected token %q", t.Lexeme)
}

// parseObjectLit parses `{key: value, key: value, ...}` —— the options
// bag form. value is restricted to literals: string / number / ident
// (where ident is interpreted as "true" / "false" / a named symbol).
// Trailing comma allowed. Empty `{}` allowed.
//
// See docs/design/command-bar-followup.md §3.1.
func (p *parser) parseObjectLit() (ast.Expr, error) {
	openTok := p.bump() // consume '{'
	pairs := []ast.Pair{}
	if p.peek().Kind == tkRBrace {
		p.bump()
		return ast.ObjectLit{Pairs: pairs}, nil
	}
	for {
		// key
		if p.peek().Kind != tkIdent {
			return nil, p.errf(p.peek().Offset, "expected key identifier in options bag (got %q)", p.peek().Lexeme)
		}
		keyTok := p.bump()
		if p.peek().Kind != tkColon {
			return nil, p.errf(p.peek().Offset, "expected ':' after key %q", keyTok.Lexeme)
		}
		p.bump() // ':'
		// value (literal only)
		val, err := p.parseModifierValue()
		if err != nil {
			return nil, err
		}
		pairs = append(pairs, ast.Pair{Key: keyTok.Lexeme, Value: val})
		if p.peek().Kind != tkComma {
			break
		}
		p.bump() // ','
		// Trailing comma before '}' is allowed.
		if p.peek().Kind == tkRBrace {
			break
		}
	}
	if p.peek().Kind != tkRBrace {
		return nil, p.errf(p.peek().Offset, "expected '}' to close options bag (opened at offset %d)", openTok.Offset)
	}
	p.bump() // '}'
	return ast.ObjectLit{Pairs: pairs}, nil
}

// parseModifierValue restricts the value form inside an options bag to
// literals. Nested calls / nested objects are rejected at parse time so
// the modifier map remains a flat key→literal projection.
func (p *parser) parseModifierValue() (ast.Expr, error) {
	t := p.peek()
	switch t.Kind {
	case tkString:
		p.bump()
		return buildStringLit(t.Parts)
	case tkNumber:
		p.bump()
		return ast.NumberLit{Value: t.Number, Raw: t.Lexeme}, nil
	case tkIdent:
		p.bump()
		// Reject namespaced idents in modifier value position to keep the
		// options-bag projection narrow.
		if p.peek().Kind == tkDot {
			return nil, p.errf(p.peek().Offset, "namespaced ident not allowed as modifier value")
		}
		if p.peek().Kind == tkLParen {
			return nil, p.errf(p.peek().Offset, "call not allowed as modifier value")
		}
		return ast.Ident{Name: t.Lexeme}, nil
	}
	return nil, p.errf(t.Offset, "expected literal value (string/number/true/false/ident) in options bag, got %q", t.Lexeme)
}

// Errors exported for callers that need to distinguish parse failure.
var ErrEmpty = errors.New("empty phrase")
