package eval_test

import (
	"strings"
	"testing"
	"time"

	"github.com/huanfeng/wind_input/internal/cmdbar"
	"github.com/huanfeng/wind_input/internal/cmdbar/eval"
	"github.com/huanfeng/wind_input/internal/cmdbar/funcs"
	"github.com/huanfeng/wind_input/internal/cmdbar/parser"
)

func mustParse(t *testing.T, src string) (display string, actions int, err error) {
	t.Helper()
	c := cmdbar.NewMemoryContext()
	c.Clock = time.Date(2026, 5, 12, 9, 30, 45, 0, time.Local)
	c.InputStr = "bdshanghai"
	c.History.Push("first")
	c.History.Push("second")
	c.ClipStr = "myclip"
	c.SelStr = "selection"
	c.AppName = "notepad.exe"
	c.TitleStr = "Untitled - Notepad"

	ph, err := parser.Parse(src)
	if err != nil {
		return "", 0, err
	}
	d, acts, err := eval.Evaluate(ph, c, cmdbar.DefaultRegistry)
	if err != nil {
		return "", 0, err
	}
	return d, len(acts), nil
}

func TestEvaluate_DesignExamples(t *testing.T) {
	cases := []struct {
		name    string
		src     string
		display string
		actions int
	}{
		{"ocbd", `$CC("打开百度", open("https://baidu.com"))`, "打开百度", 1},
		{"bd", `$CC("百度搜索 {tail(code,3)}", open("https://www.baidu.com/s?wd={url(tail(code,3))}"))`, "百度搜索 shanghai", 1},
		{"z_call", `$CC(last(), type(last()))`, "second", 1},
		{"z_ident", `$CC(last, type(last))`, "second", 1},
		{"jiao", `$CC("《》", type("《》"), key.tap("Left"))`, "《》", 2},
		{"dl", `$CC("[删行]", key.seq("Home", "Shift+End", "Backspace"))`, "[删行]", 1},
		{"zd", `$CC("汉典 · {last(1)}", open("https://www.zdic.net/hans/{url(last(1))}"))`, "汉典 · second", 1},
		{"addc", `$CC("加词 · {clip()}", dict.addword(clip()))`, "加词 · myclip", 1},
		{"addl", `$CC("收藏 · {last()}", dict.addword(last()))`, "收藏 · second", 1},
		{"calc_cmd", `$CC("= {calc('1+2*3')}", type(calc('1+2*3')))`, "= 7", 1},
		{"ip", `$CC("IP", shell("curl"), clip.paste())`, "IP", 2},
		{"now_template", `{date('YYYY-MM-DD')} {time('HH:mm:ss')}`, "2026-05-12 09:30:45", 0},
		{"literal", "just text", "just text", 0},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			d, a, err := mustParse(t, c.src)
			if err != nil {
				t.Fatalf("err: %v", err)
			}
			if d != c.display {
				t.Errorf("display = %q want %q", d, c.display)
			}
			if a != c.actions {
				t.Errorf("actions = %d want %d", a, c.actions)
			}
		})
	}
}

func TestEvaluate_DisplayRejectsImpure(t *testing.T) {
	// Display referencing a side-effect function must fail.
	cases := []string{
		`$CC(open("https://x.com"))`,
		`$CC("{type('x')}")`,
		`$CC(type("x"))`,
	}
	for _, src := range cases {
		t.Run(src, func(t *testing.T) {
			_, _, err := mustParse(t, src)
			if err == nil {
				t.Fatalf("want error for %q", src)
			}
			if !strings.Contains(err.Error(), "display:") {
				t.Errorf("err missing display: prefix: %v", err)
			}
		})
	}
}

// TestEvaluate_ActionKind 验证 P5 的 ResolvedAction 模型: type(...) 解析为
// ActionText, 其它动作解析为 ActionEffect; 多个 type 与 effect 交错时 Kind
// 排列与源码一致。
func TestEvaluate_ActionKind(t *testing.T) {
	r := buildActionsRegistry()
	c := cmdbar.NewMemoryContext()
	c.Svcs = &cmdbar.Services{Keys: &fakeKeys{}}
	ph, err := parser.Parse(`$CC("d", type("a"), key.tap("X"), type("b"))`)
	if err != nil {
		t.Fatal(err)
	}
	_, acts, err := eval.Evaluate(ph, c, r)
	if err != nil {
		t.Fatal(err)
	}
	if len(acts) != 3 {
		t.Fatalf("acts = %d, want 3", len(acts))
	}
	if acts[0].Kind != cmdbar.ActionText {
		t.Errorf("acts[0].Kind = %v, want ActionText", acts[0].Kind)
	}
	if acts[1].Kind != cmdbar.ActionEffect {
		t.Errorf("acts[1].Kind = %v, want ActionEffect", acts[1].Kind)
	}
	if acts[2].Kind != cmdbar.ActionText {
		t.Errorf("acts[2].Kind = %v, want ActionText", acts[2].Kind)
	}

	// Text Run() 返回真实文本; Effect Run() 返回 ""。
	t0, err := acts[0].Run()
	if err != nil || t0 != "a" {
		t.Errorf("acts[0].Run() = %q, %v, want \"a\", nil", t0, err)
	}
	t2, err := acts[2].Run()
	if err != nil || t2 != "b" {
		t.Errorf("acts[2].Run() = %q, %v, want \"b\", nil", t2, err)
	}
	if e, err := acts[1].Run(); err != nil || e != "" {
		t.Errorf("acts[1].Run() = %q, %v, want \"\", nil", e, err)
	}
}

// TestEvaluate_TypeBypassesRegistry 验证 type(...) 不再走 registry Lookup,
// 即使 registry 没注册 type 也能解析并产出 ActionText。
func TestEvaluate_TypeBypassesRegistry(t *testing.T) {
	r := cmdbar.NewRegistry()
	// 仅注册纯函数 + open (用于显示名检查不会拒); 不注册 type。
	for _, name := range cmdbar.DefaultRegistry.Names() {
		if name == "type" {
			continue
		}
		if spec, ok := cmdbar.DefaultRegistry.Lookup(name); ok {
			r.Register(spec)
		}
	}
	ph, err := parser.Parse(`$CC("hi", type("hello"))`)
	if err != nil {
		t.Fatal(err)
	}
	c := cmdbar.NewMemoryContext()
	_, acts, err := eval.Evaluate(ph, c, r)
	if err != nil {
		t.Fatalf("evaluate err: %v", err)
	}
	if len(acts) != 1 || acts[0].Kind != cmdbar.ActionText {
		t.Fatalf("acts = %+v", acts)
	}
	got, err := acts[0].Run()
	if err != nil || got != "hello" {
		t.Errorf("acts[0].Run() = %q, %v", got, err)
	}
}

// fakeOpen records Open calls for thunk-invocation assertions below.
type fakeOpen struct{ urls []string }

func (f *fakeOpen) Open(t string) error { f.urls = append(f.urls, t); return nil }

type fakeKeys struct {
	taps []string
	seqs [][]string
}

func (f *fakeKeys) Tap(c string) error { f.taps = append(f.taps, c); return nil }
func (f *fakeKeys) Sequence(cs ...string) error {
	f.seqs = append(f.seqs, append([]string(nil), cs...))
	return nil
}
func (f *fakeKeys) Hold(c string) error     { return nil }
func (f *fakeKeys) Release(c string) error  { return nil }
func (f *fakeKeys) TypeText(s string) error { return nil }

type fakeClip struct{ set string }

func (f *fakeClip) SetText(s string) error   { f.set = s; return nil }
func (f *fakeClip) GetText() (string, error) { return "", nil }
func (f *fakeClip) Paste() error             { return nil }

// buildActionsRegistry returns a registry preloaded with every
// function (pure + action) so eval tests can route actions through real
// implementations.
func buildActionsRegistry() *cmdbar.Registry {
	r := cmdbar.NewRegistry()
	// Copy every entry from the default (pure + stubs) registry.
	for _, name := range cmdbar.DefaultRegistry.Names() {
		if spec, ok := cmdbar.DefaultRegistry.Lookup(name); ok {
			r.Register(spec)
		}
	}
	funcs.RegisterActions(r)
	return r
}

func TestEvaluate_Action_Open_InvokesService(t *testing.T) {
	r := buildActionsRegistry()
	fo := &fakeOpen{}
	c := cmdbar.NewMemoryContext()
	c.Svcs = &cmdbar.Services{Open: fo}
	c.InputStr = "bdshanghai"

	ph, err := parser.Parse(`$CC("百度 {tail(code,3)}", open("https://www.baidu.com/s?wd={url(tail(code,3))}"))`)
	if err != nil {
		t.Fatal(err)
	}
	disp, acts, err := eval.Evaluate(ph, c, r)
	if err != nil {
		t.Fatal(err)
	}
	if disp != "百度 shanghai" {
		t.Errorf("disp = %q", disp)
	}
	if len(acts) != 1 {
		t.Fatalf("acts = %d", len(acts))
	}
	if _, err := acts[0].Run(); err != nil {
		t.Fatalf("run: %v", err)
	}
	if len(fo.urls) != 1 || fo.urls[0] != "https://www.baidu.com/s?wd=shanghai" {
		t.Errorf("Open urls = %v", fo.urls)
	}
}

func TestEvaluate_Action_LazyArgReevaluation(t *testing.T) {
	// type 的 arg 表达式必须在 Run 时再求值, 这样 `type(last())` 能反映
	// commit 后的 history。
	r := buildActionsRegistry()
	c := cmdbar.NewMemoryContext()
	c.History.Push("alpha")

	ph, err := parser.Parse(`$CC(last(), type(last()))`)
	if err != nil {
		t.Fatal(err)
	}
	disp, acts, err := eval.Evaluate(ph, c, r)
	if err != nil {
		t.Fatal(err)
	}
	if disp != "alpha" {
		t.Errorf("disp = %q", disp)
	}
	// Mutate history before firing Run.
	c.History.Push("beta")
	if acts[0].Kind != cmdbar.ActionText {
		t.Fatalf("expected type → ActionText, got %v", acts[0].Kind)
	}
	got, err := acts[0].Run()
	if err != nil {
		t.Fatal(err)
	}
	if got != "beta" {
		t.Errorf("type Run after history.Push: got %q, want %q (lazy re-eval)", got, "beta")
	}
}

func TestEvaluate_Action_DesignExample_JiaoBracket(t *testing.T) {
	// §3.6 `《` → display "《》", actions: type("《》"), key.tap("Left")。
	r := buildActionsRegistry()
	fk := &fakeKeys{}
	c := cmdbar.NewMemoryContext()
	c.Svcs = &cmdbar.Services{Keys: fk}
	ph, err := parser.Parse(`$CC("《》", type("《》"), key.tap("Left"))`)
	if err != nil {
		t.Fatal(err)
	}
	_, acts, err := eval.Evaluate(ph, c, r)
	if err != nil {
		t.Fatal(err)
	}
	if len(acts) != 2 {
		t.Fatalf("acts = %d", len(acts))
	}
	if acts[0].Kind != cmdbar.ActionText {
		t.Fatalf("acts[0].Kind want ActionText, got %v", acts[0].Kind)
	}
	if acts[1].Kind != cmdbar.ActionEffect {
		t.Fatalf("acts[1].Kind want ActionEffect, got %v", acts[1].Kind)
	}
	txt, err := acts[0].Run()
	if err != nil || txt != "《》" {
		t.Errorf("acts[0].Run = %q, %v", txt, err)
	}
	if _, err := acts[1].Run(); err != nil {
		t.Fatal(err)
	}
	if len(fk.taps) != 1 || fk.taps[0] != "Left" {
		t.Errorf("taps = %v", fk.taps)
	}
}

func TestEvaluate_Action_DesignExample_DeleteLine(t *testing.T) {
	// §3.6 `dl` → key.seq("Home", "Shift+End", "Backspace")。
	r := buildActionsRegistry()
	fk := &fakeKeys{}
	c := cmdbar.NewMemoryContext()
	c.Svcs = &cmdbar.Services{Keys: fk}
	ph, err := parser.Parse(`$CC("[删行]", key.seq("Home", "Shift+End", "Backspace"))`)
	if err != nil {
		t.Fatal(err)
	}
	_, acts, err := eval.Evaluate(ph, c, r)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := acts[0].Run(); err != nil {
		t.Fatal(err)
	}
	if len(fk.seqs) != 1 || len(fk.seqs[0]) != 3 || fk.seqs[0][2] != "Backspace" {
		t.Errorf("seqs = %v", fk.seqs)
	}
}

func TestEvaluate_CalcInDisplay(t *testing.T) {
	// §9 acceptance case #9: `cal1+2*3` → 7. We exercise the
	// calc-as-display path explicitly.
	d, _, err := mustParse(t, `$CC("{calc('1+2*3')}")`)
	if err != nil {
		t.Fatal(err)
	}
	if d != "7" {
		t.Errorf("display = %q want 7", d)
	}
}

// fakeClip kept for backwards-compat with anyone still referencing it; we
// can't drop it because gofmt warns about unused imports if so. Keep it
// referenced via _.
var _ = fakeClip{}
