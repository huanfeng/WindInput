package parser

import (
	"testing"
	"time"
)

// timeAfter 给 Tokenize 死循环回归测试一个 2 秒上限; 正常单次 Tokenize 是
// 微秒级。变量化便于未来按 CI 慢机调整。
func timeAfter() <-chan time.Time { return time.After(2 * time.Second) }

// TestLexer_FullWidthQuoteDoesNotInfiniteLoop 是 2026-05-18 OOM 事故的回归测试。
// 历史 bug: dispatch 用 `isIdentStart(rune(c))` 单字节 cast, 多字节 UTF-8 首字节
// (如全角引号 `"` = E2 80 9C 的 0xE2 = U+00E2 'â') 被误判为字母 → 进入 scanIdent →
// scanIdent 用 utf8.DecodeRuneInString 正确解码 U+201C, 发现不是 ident-cont 立刻
// break → l.pos 不前进 → 外层 for 死循环 + 无限 append → 内存几秒涨到 10GB+。
//
// 修复后: 非 ASCII 字节走 default 分支, 返回 "unexpected character" 错误。
func TestLexer_FullWidthQuoteDoesNotInfiniteLoop(t *testing.T) {
	src := "“百度”" // “百度” (全角引号包中文)
	done := make(chan struct{})
	var toks []Token
	var err error
	go func() {
		toks, err = NewLexer(src).Tokenize()
		close(done)
	}()
	select {
	case <-done:
		if err == nil {
			t.Fatalf("expected error on full-width quote input, got tokens=%v", toks)
		}
	case <-timeAfter():
		t.Fatal("Tokenize did not terminate: lexer is in an infinite loop (regression of full-width quote OOM bug)")
	}
}

// TestLexer_NonASCIIBytesReportedAsError 验证常见非 ASCII 首字节 (中文字符、全角
// 标点) 在表达式上下文都能稳定报错, 而不是误进 ident 路径。
func TestLexer_NonASCIIBytesReportedAsError(t *testing.T) {
	cases := []string{
		"“",      // “
		"”",      // ”
		"中",      // 中
		"（",      // （ 全角左括号
		"foo(“)", // foo("
	}
	for _, src := range cases {
		_, err := NewLexer(src).Tokenize()
		if err == nil {
			t.Errorf("expected error tokenizing %q, got nil", src)
		}
	}
}

func TestLexer_BasicTokens(t *testing.T) {
	cases := []struct {
		name string
		src  string
		want []TokenKind
	}{
		{"empty", "", []TokenKind{tkEOF}},
		{"paren", "()", []TokenKind{tkLParen, tkRParen, tkEOF}},
		{"comma", ",", []TokenKind{tkComma, tkEOF}},
		{"dot", ".", []TokenKind{tkDot, tkEOF}},
		{"ident", "foo", []TokenKind{tkIdent, tkEOF}},
		{"call", "foo(1, 2)", []TokenKind{tkIdent, tkLParen, tkNumber, tkComma, tkNumber, tkRParen, tkEOF}},
		{"ns_call", "clip.copy()", []TokenKind{tkIdent, tkDot, tkIdent, tkLParen, tkRParen, tkEOF}},
		{"string", `"hi"`, []TokenKind{tkString, tkEOF}},
		{"neg_num", "-1.5", []TokenKind{tkNumber, tkEOF}},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			toks, err := NewLexer(c.src).Tokenize()
			if err != nil {
				t.Fatalf("unexpected err: %v", err)
			}
			if len(toks) != len(c.want) {
				t.Fatalf("want %d tokens, got %d: %+v", len(c.want), len(toks), toks)
			}
			for i, k := range c.want {
				if toks[i].Kind != k {
					t.Fatalf("token %d: want %v got %v (%q)", i, k, toks[i].Kind, toks[i].Lexeme)
				}
			}
		})
	}
}

func TestLexer_StringInterp(t *testing.T) {
	toks, err := NewLexer(`"hello {name}!"`).Tokenize()
	if err != nil {
		t.Fatalf("err: %v", err)
	}
	if toks[0].Kind != tkString {
		t.Fatalf("want string, got %v", toks[0].Kind)
	}
	parts := toks[0].Parts
	if len(parts) != 3 {
		t.Fatalf("want 3 parts, got %d: %+v", len(parts), parts)
	}
	if parts[0].IsInterp || parts[0].Lit != "hello " {
		t.Errorf("part0 = %+v", parts[0])
	}
	if !parts[1].IsInterp || parts[1].Interp != "name" {
		t.Errorf("part1 = %+v", parts[1])
	}
	if parts[2].IsInterp || parts[2].Lit != "!" {
		t.Errorf("part2 = %+v", parts[2])
	}
}

func TestLexer_StringEscapes(t *testing.T) {
	toks, err := NewLexer(`"a\"b\\c\{d\}"`).Tokenize()
	if err != nil {
		t.Fatalf("err: %v", err)
	}
	if len(toks[0].Parts) != 1 {
		t.Fatalf("want 1 part, got %+v", toks[0].Parts)
	}
	if got := toks[0].Parts[0].Lit; got != `a"b\c{d}` {
		t.Errorf("decoded = %q", got)
	}
}

func TestLexer_Errors(t *testing.T) {
	cases := []string{
		`"unterminated`,
		`"bad {nested"`,
		`"oops}"`,
		`@`,
	}
	for _, src := range cases {
		t.Run(src, func(t *testing.T) {
			if _, err := NewLexer(src).Tokenize(); err == nil {
				t.Errorf("want error for %q", src)
			}
		})
	}
}

func TestScanString_UnknownEscapeLenient(t *testing.T) {
	cases := []struct {
		name string
		src  string
		want string
	}{
		{
			// 未知转义 \p 不再报错, 原样保留为反斜杠 + p。
			name: "unknown_escape_preserved",
			src:  `"C:\path"`,
			want: `C:\path`,
		},
		{
			// \n 是已知转义, 宽松策略下路径里的 \n 会被解释为换行符 (已接受行为)。
			name: "known_escape_newline_in_path",
			src:  `"C:\new"`,
			want: "C:\new",
		},
		{
			// 已知转义 \t 正常解析为 TAB。
			name: "known_escape_tab",
			src:  `"a\tb"`,
			want: "a\tb",
		},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			toks, err := NewLexer(c.src).Tokenize()
			if err != nil {
				t.Fatalf("Tokenize returned error: %v", err)
			}
			if toks[0].Kind != tkString {
				t.Fatalf("token 0 kind = %v, want tkString", toks[0].Kind)
			}
			var lit string
			for _, p := range toks[0].Parts {
				if !p.IsInterp {
					lit += p.Lit
				}
			}
			if lit != c.want {
				t.Fatalf("decoded literal = %q, want %q", lit, c.want)
			}
		})
	}
}
