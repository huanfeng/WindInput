package parser

import (
	"strings"
	"testing"

	"github.com/huanfeng/wind_input/internal/cmdbar/ast"
)

func TestParse_Literal(t *testing.T) {
	p, err := Parse("hello world")
	if err != nil {
		t.Fatalf("err: %v", err)
	}
	lp, ok := p.(ast.LiteralPhrase)
	if !ok {
		t.Fatalf("want LiteralPhrase, got %T", p)
	}
	if lp.Text != "hello world" {
		t.Errorf("text = %q", lp.Text)
	}
}

func TestParse_Template(t *testing.T) {
	p, err := Parse("Hi {name}!")
	if err != nil {
		t.Fatalf("err: %v", err)
	}
	tp, ok := p.(ast.TemplatePhrase)
	if !ok {
		t.Fatalf("want TemplatePhrase, got %T", p)
	}
	sl, ok := tp.Expr.(ast.StringLit)
	if !ok {
		t.Fatalf("want StringLit expr, got %T", tp.Expr)
	}
	if len(sl.Parts) != 3 {
		t.Fatalf("want 3 parts, got %d", len(sl.Parts))
	}
	if _, ok := sl.Parts[1].(ast.InterpPart); !ok {
		t.Errorf("part1 type = %T", sl.Parts[1])
	}
}

func TestParse_Command_Examples(t *testing.T) {
	// §3.6 example table. We assert the phrase parses and is a
	// CommandPhrase / LiteralPhrase / TemplatePhrase as appropriate,
	// then spot-check display + action counts.
	cases := []struct {
		name    string
		src     string
		display string
		actions int
	}{
		{"ocbd", `$CC("打开百度", open("https://baidu.com"))`, `"打开百度"`, 1},
		{"bd", `$CC("百度搜索 {tail(code,2)}", open("https://www.baidu.com/s?wd={url(tail(code,2))}"))`, `"百度搜索 {tail(code, 2)}"`, 1},
		{"z", `$CC(last(), type(last()))`, `last()`, 1},
		{"jiao", `$CC("《》", type("《》"), key.tap("Left"))`, `"《》"`, 2},
		{"dl", `$CC("[删行]", key.seq("Home", "Shift+End", "Backspace"))`, `"[删行]"`, 1},
		{"zd", `$CC("汉典 · {last(1)}", open("https://www.zdic.net/hans/{url(last(1))}"))`, `"汉典 · {last(1)}"`, 1},
		{"addc", `$CC("加词 · {clip()}", dict.addword(clip()))`, `"加词 · {clip()}"`, 1},
		{"addl", `$CC("收藏 · {last()}", dict.addword(last()))`, `"收藏 · {last()}"`, 1},
		{"calc_cmd", `$CC("= {calc(tail(code,2))}", type(calc(tail(code,2))))`, `"= {calc(tail(code, 2))}"`, 1},
		{"ip", `$CC("IP", shell("curl -s https://api.ipify.org > %TEMP%\\ip.txt"), clip.paste())`, `"IP"`, 2},
		{"no_action", `$CC("just display")`, `"just display"`, 0},
		{"ident_zero_arg", `$CC(last)`, `last`, 0},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			p, err := Parse(c.src)
			if err != nil {
				t.Fatalf("err: %v", err)
			}
			cp, ok := p.(ast.CommandPhrase)
			if !ok {
				t.Fatalf("want CommandPhrase, got %T", p)
			}
			if got := cp.Display.String(); got != c.display {
				t.Errorf("display = %q want %q", got, c.display)
			}
			if len(cp.Actions) != c.actions {
				t.Errorf("actions = %d want %d", len(cp.Actions), c.actions)
			}
		})
	}
}

func TestParse_TemplatePhrase_NowExample(t *testing.T) {
	// "now" example from §3.6: "{date('YYYY-MM-DD')} {time('HH:mm:ss')}"
	src := `{date('YYYY-MM-DD')} {time('HH:mm:ss')}`
	p, err := Parse(src)
	if err != nil {
		t.Fatalf("err: %v", err)
	}
	tp, ok := p.(ast.TemplatePhrase)
	if !ok {
		t.Fatalf("want TemplatePhrase, got %T", p)
	}
	sl := tp.Expr.(ast.StringLit)
	if len(sl.Parts) != 3 {
		t.Fatalf("parts = %d want 3", len(sl.Parts))
	}
}

func TestParse_BareIdentEqualsZeroArgCall(t *testing.T) {
	// Inside a $CC, `last` and `last()` must produce equivalent ASTs in
	// terms of evaluator behavior. We assert one is Ident and the other
	// is Call, leaving the equivalence to the evaluator layer.
	p1, err := Parse(`$CC(last)`)
	if err != nil {
		t.Fatal(err)
	}
	p2, err := Parse(`$CC(last())`)
	if err != nil {
		t.Fatal(err)
	}
	if _, ok := p1.(ast.CommandPhrase).Display.(ast.Ident); !ok {
		t.Errorf("p1 display = %T", p1.(ast.CommandPhrase).Display)
	}
	if _, ok := p2.(ast.CommandPhrase).Display.(ast.Call); !ok {
		t.Errorf("p2 display = %T", p2.(ast.CommandPhrase).Display)
	}
}

func TestParse_Errors(t *testing.T) {
	cases := []struct {
		name    string
		src     string
		wantSub string
	}{
		{"unclosed_paren", `$CC("x", open("`, "unclosed"},
		{"unclosed_string", `$CC("x)`, ""},
		{"triple_dot", `$CC(a.b.c())`, "most one"},
		{"trailing_comma", `$CC("x",)`, "trailing"},
		{"empty_cc", `$CC()`, "display"},
		{"text_before_cc", `pre$CC("x")`, "unexpected text"},
		{"text_after_cc", `$CC("x") tail`, "unexpected text"},
		{"namespaced_bare", `$CC(clip.copy)`, "must be called"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			_, err := Parse(c.src)
			if err == nil {
				t.Fatalf("want error for %q", c.src)
			}
			if c.wantSub != "" && !strings.Contains(err.Error(), c.wantSub) {
				t.Errorf("err %q missing substring %q", err.Error(), c.wantSub)
			}
		})
	}
}

func TestParse_StringInterpNested(t *testing.T) {
	p, err := Parse(`$CC("{url(tail(code, 2))}")`)
	if err != nil {
		t.Fatalf("err: %v", err)
	}
	cp := p.(ast.CommandPhrase)
	sl := cp.Display.(ast.StringLit)
	if len(sl.Parts) != 1 {
		t.Fatalf("want 1 part, got %d", len(sl.Parts))
	}
	ip, ok := sl.Parts[0].(ast.InterpPart)
	if !ok {
		t.Fatalf("want InterpPart, got %T", sl.Parts[0])
	}
	call, ok := ip.Expr.(ast.Call)
	if !ok || call.Name != "url" {
		t.Errorf("want url(...), got %T %v", ip.Expr, ip.Expr)
	}
}

// TestParse_CC1Marker 验证 `$CC1(` marker 解析为与 `$CC(` 完全等价的 CommandPhrase。
// 唯一差别由前缀过滤层 (dict.IsCmdbarExactOnly) 处理, 不进 AST。
func TestParse_CC1Marker(t *testing.T) {
	p, err := Parse(`$CC1("打开百度", open("https://baidu.com"))`)
	if err != nil {
		t.Fatalf("err: %v", err)
	}
	cp, ok := p.(ast.CommandPhrase)
	if !ok {
		t.Fatalf("want CommandPhrase, got %T", p)
	}
	if len(cp.Actions) != 1 {
		t.Errorf("want 1 action, got %d", len(cp.Actions))
	}
	// display 应为字面 "打开百度"
	sl, ok := cp.Display.(ast.StringLit)
	if !ok {
		t.Fatalf("want display StringLit, got %T", cp.Display)
	}
	if len(sl.Parts) != 1 {
		t.Fatalf("want display 1 part, got %d", len(sl.Parts))
	}
	if lp, ok := sl.Parts[0].(ast.LiteralPart); !ok || lp.Text != "打开百度" {
		t.Errorf("display literal mismatch: %+v", sl.Parts[0])
	}
}

// TestParse_CC1Precedence 验证 `$CC1(` 必须先于 `$CC(` 匹配, 否则
// "$CC1(...)" 会被吃成 "$CC(" + 残留 "1(", 导致解析错位。
func TestParse_CC1Precedence(t *testing.T) {
	// 这条短语合法且只有一个 action; 若 marker 切分错误, action 计数或解析 err 会变。
	p, err := Parse(`$CC1("x", open("y"))`)
	if err != nil {
		t.Fatalf("err: %v", err)
	}
	cp, ok := p.(ast.CommandPhrase)
	if !ok {
		t.Fatalf("want CommandPhrase, got %T", p)
	}
	if len(cp.Actions) != 1 {
		t.Errorf("want 1 action, got %d", len(cp.Actions))
	}
}

func TestParseTemplatePhrase_UnknownEscapeLenient(t *testing.T) {
	// 模板短语 (含 {expr}) 里的未知转义 \p 不再报错。
	ph, err := Parse(`路径 C:\path 时间 {now}`)
	if err != nil {
		t.Fatalf("Parse returned error: %v", err)
	}
	tp, ok := ph.(ast.TemplatePhrase)
	if !ok {
		t.Fatalf("Parse returned %T, want ast.TemplatePhrase", ph)
	}
	sl, ok := tp.Expr.(ast.StringLit)
	if !ok {
		t.Fatalf("template expr is %T, want ast.StringLit", tp.Expr)
	}
	var lit string
	for _, part := range sl.Parts {
		if lp, ok := part.(ast.LiteralPart); ok {
			lit += lp.Text
		}
	}
	if lit != `路径 C:\path 时间 ` {
		t.Fatalf("decoded literal parts = %q, want %q", lit, `路径 C:\path 时间 `)
	}
}
