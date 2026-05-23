// Package parser implements the command bar lexer and recursive-descent
// parser. See docs/design/command-bar-design.md §2.
package parser

import (
	"fmt"
	"unicode"
	"unicode/utf8"
)

// TokenKind enumerates the lexical token classes produced by the lexer.
type TokenKind int

const (
	tkEOF TokenKind = iota
	tkIdent
	tkNumber
	tkString // STRING token carries the already-decoded parts via lexer state
	tkLParen
	tkRParen
	tkComma
	tkDot
	tkLBrace   // '{' — opens an options-bag ObjectLit at expression position
	tkRBrace   // '}' — closes an options-bag ObjectLit
	tkColon    // ':' — separates key from value inside an ObjectLit
	tkDollarCC // literal "$CC" prefix; only emitted at top-level scan
)

// stringPart represents a decoded segment of a string literal. Either
// Lit is non-empty (literal text) or Tokens holds the nested tokens for
// an interpolation.
type stringPart struct {
	IsInterp bool
	Lit      string
	// Interp text: the raw source between the matching `{` and `}`, to be
	// re-parsed as an expression by the parser layer.
	Interp string
	// Offset of this part within the source (for error reporting).
	Offset int
}

// Token is a lexical token.
type Token struct {
	Kind   TokenKind
	Lexeme string       // raw text
	Number float64      // valid when Kind == tkNumber
	Parts  []stringPart // valid when Kind == tkString
	Offset int          // byte offset in source where this token starts
}

// Lexer scans the expression-level subset of the command-bar grammar.
// String contents are scanned together with the surrounding quotes; the
// returned tkString token includes a decoded Parts slice that captures
// literal segments and `{expr}` interpolation bodies as raw substrings.
type Lexer struct {
	src    string
	pos    int // byte position
	tokens []Token
}

// NewLexer constructs a Lexer over src. Call Tokenize once to obtain the
// full token slice.
func NewLexer(src string) *Lexer {
	return &Lexer{src: src}
}

// Tokenize consumes the entire input and returns the token slice.
// String literals are decoded inline; identifiers and numbers obey the
// usual rules. A trailing tkEOF token is always appended.
func (l *Lexer) Tokenize() ([]Token, error) {
	for l.pos < len(l.src) {
		c := l.src[l.pos]
		switch {
		case c == ' ' || c == '\t' || c == '\n' || c == '\r':
			l.pos++
		case c == '(':
			l.tokens = append(l.tokens, Token{Kind: tkLParen, Lexeme: "(", Offset: l.pos})
			l.pos++
		case c == ')':
			l.tokens = append(l.tokens, Token{Kind: tkRParen, Lexeme: ")", Offset: l.pos})
			l.pos++
		case c == ',':
			l.tokens = append(l.tokens, Token{Kind: tkComma, Lexeme: ",", Offset: l.pos})
			l.pos++
		case c == '.':
			l.tokens = append(l.tokens, Token{Kind: tkDot, Lexeme: ".", Offset: l.pos})
			l.pos++
		case c == '{':
			// '{' at expression position opens an options-bag ObjectLit.
			// Inside a string literal, `{...}` is handled by scanString as
			// an interpolation; that path never reaches this branch.
			l.tokens = append(l.tokens, Token{Kind: tkLBrace, Lexeme: "{", Offset: l.pos})
			l.pos++
		case c == '}':
			l.tokens = append(l.tokens, Token{Kind: tkRBrace, Lexeme: "}", Offset: l.pos})
			l.pos++
		case c == ':':
			l.tokens = append(l.tokens, Token{Kind: tkColon, Lexeme: ":", Offset: l.pos})
			l.pos++
		case c == '"' || c == '\'':
			tok, err := l.scanString(c)
			if err != nil {
				return nil, err
			}
			l.tokens = append(l.tokens, tok)
		case c == '-' || (c >= '0' && c <= '9'):
			tok, ok, err := l.tryScanNumber()
			if err != nil {
				return nil, err
			}
			if ok {
				l.tokens = append(l.tokens, tok)
				continue
			}
			return nil, fmt.Errorf("unexpected character %q at offset %d", c, l.pos)
		case isASCIIIdentStart(c):
			// 仅用 ASCII 字节判定 ident 起始 (函数名/关键字一律 ASCII)。
			// 历史 bug: 这里曾用 `isIdentStart(rune(c))`, 把单字节直接 cast 成 rune,
			// 对 UTF-8 多字节首字节 (如全角引号 `"` = E2 80 9C 的首字节 0xE2 = 'â')
			// 命中 unicode.IsLetter, 进入 scanIdent; 而 scanIdent 用 utf8.DecodeRuneInString
			// 正确解码后发现不是 ident-cont, 立即 break, 不前进 pos —— 外层 for 循环
			// 永远看到同一字节, 形成无限 append 死循环, 内存爆涨。
			// 详见 docs/design/command-bar-followup.md (Lexer 字节/UTF-8 边界一致性)。
			tok := l.scanIdent()
			l.tokens = append(l.tokens, tok)
		default:
			r, _ := utf8.DecodeRuneInString(l.src[l.pos:])
			return nil, fmt.Errorf("unexpected character %q at offset %d", r, l.pos)
		}
	}
	l.tokens = append(l.tokens, Token{Kind: tkEOF, Offset: l.pos})
	return l.tokens, nil
}

// isASCIIIdentStart 只判定 ASCII 字节是否为 ident 起始。dispatch 阶段必须用
// 字节级判定 (不能 rune(c) 单字节 cast), 否则 UTF-8 多字节首字节会被误判为
// Latin Supplement 字母, 与 scanIdent 内部的 utf8.DecodeRuneInString 视角冲突,
// 产生 "判进入但不消费" 的死循环。
func isASCIIIdentStart(c byte) bool {
	return c == '_' || (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')
}

func isIdentCont(r rune) bool {
	return r == '_' || unicode.IsLetter(r) || unicode.IsDigit(r)
}

func (l *Lexer) scanIdent() Token {
	start := l.pos
	for l.pos < len(l.src) {
		r, sz := utf8.DecodeRuneInString(l.src[l.pos:])
		if !isIdentCont(r) {
			break
		}
		l.pos += sz
	}
	// 防御: 若调用方误判 ident 起始导致 scanIdent 不前进, 强制吞一个字节
	// 让 Tokenize 收尾时报 "empty identifier" 错误而不是死循环。dispatch 已用
	// isASCIIIdentStart 收紧, 这条分支正常不应触达。
	if l.pos == start && start < len(l.src) {
		l.pos++
	}
	return Token{Kind: tkIdent, Lexeme: l.src[start:l.pos], Offset: start}
}

func (l *Lexer) tryScanNumber() (Token, bool, error) {
	start := l.pos
	p := l.pos
	if p < len(l.src) && l.src[p] == '-' {
		// Bare '-' is not a token; only accept as part of a number if a
		// digit follows. Otherwise reject so the caller can surface a
		// useful error.
		if p+1 >= len(l.src) || !(l.src[p+1] >= '0' && l.src[p+1] <= '9') {
			return Token{}, false, fmt.Errorf("unexpected '-' at offset %d", l.pos)
		}
		p++
	}
	for p < len(l.src) && l.src[p] >= '0' && l.src[p] <= '9' {
		p++
	}
	if p < len(l.src) && l.src[p] == '.' {
		p++
		dStart := p
		for p < len(l.src) && l.src[p] >= '0' && l.src[p] <= '9' {
			p++
		}
		if p == dStart {
			return Token{}, false, fmt.Errorf("invalid number at offset %d", start)
		}
	}
	lex := l.src[start:p]
	if lex == "" || lex == "-" {
		return Token{}, false, fmt.Errorf("invalid number at offset %d", start)
	}
	var f float64
	_, err := fmt.Sscanf(lex, "%g", &f)
	if err != nil {
		return Token{}, false, fmt.Errorf("invalid number %q at offset %d", lex, start)
	}
	l.pos = p
	return Token{Kind: tkNumber, Lexeme: lex, Number: f, Offset: start}, true, nil
}

// scanString reads a string literal terminated by quote. Inside, the
// escape sequences \" \\ \{ \} \( \) \n \t are honored. `{ ... }` blocks
// are captured as interpolation parts: brace matching honors nested
// braces (e.g. inside nested string literals).
func (l *Lexer) scanString(quote byte) (Token, error) {
	start := l.pos
	l.pos++ // consume opening quote
	var parts []stringPart
	var lit []byte
	flushLit := func(off int) {
		if len(lit) > 0 {
			parts = append(parts, stringPart{Lit: string(lit), Offset: off})
			lit = lit[:0]
		}
	}
	for l.pos < len(l.src) {
		c := l.src[l.pos]
		if c == quote {
			flushLit(l.pos)
			l.pos++ // consume closing
			return Token{Kind: tkString, Lexeme: l.src[start:l.pos], Parts: parts, Offset: start}, nil
		}
		if c == '\\' && l.pos+1 < len(l.src) {
			next := l.src[l.pos+1]
			switch next {
			case '\\', '"', '\'', '{', '}', '(', ')':
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
			l.pos += 2
			continue
		}
		if c == '{' {
			flushLit(l.pos)
			interpStart := l.pos + 1
			depth := 1
			p := interpStart
			inInner := byte(0)
			for p < len(l.src) && depth > 0 {
				ch := l.src[p]
				if inInner != 0 {
					if ch == '\\' && p+1 < len(l.src) {
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
				return Token{}, fmt.Errorf("unclosed '{' in string at offset %d", l.pos)
			}
			parts = append(parts, stringPart{IsInterp: true, Interp: l.src[interpStart:p], Offset: l.pos})
			l.pos = p + 1 // skip '}'
			continue
		}
		if c == '}' {
			return Token{}, fmt.Errorf("unmatched '}' in string at offset %d", l.pos)
		}
		lit = append(lit, c)
		l.pos++
	}
	return Token{}, fmt.Errorf("unclosed string starting at offset %d", start)
}
