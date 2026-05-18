package funcs_test

import (
	"errors"
	"strings"
	"testing"

	"github.com/huanfeng/wind_input/internal/cmdbar"
	"github.com/huanfeng/wind_input/internal/cmdbar/funcs"
)

// --- mock services ---

type mockClip struct {
	set string
	get string
	err error
}

func (m *mockClip) SetText(s string) error   { m.set = s; return m.err }
func (m *mockClip) GetText() (string, error) { return m.get, m.err }

type mockKeys struct {
	taps   []string
	seqs   [][]string
	tapErr error
}

func (m *mockKeys) Tap(c string) error {
	m.taps = append(m.taps, c)
	return m.tapErr
}
func (m *mockKeys) Sequence(cs ...string) error {
	cp := append([]string(nil), cs...)
	m.seqs = append(m.seqs, cp)
	return nil
}

type mockOpen struct {
	got []string
	err error
}

func (m *mockOpen) Open(t string) error { m.got = append(m.got, t); return m.err }

type mockProc struct {
	runs    []string   // "cmd|arg1|arg2"
	shells  []string   // 单参 Shell() 的入参 (旧接口)
	shellEx []shellCap // 两参 ShellEx() 的入参 (新接口)
}

// shellCap 记录 ShellEx 调用细节, 单测断言用。
type shellCap struct {
	cmd   string
	flags []string
}

func (m *mockProc) Run(cmd string, args ...string) error {
	parts := append([]string{cmd}, args...)
	m.runs = append(m.runs, strings.Join(parts, "|"))
	return nil
}
func (m *mockProc) Shell(c string) error { m.shells = append(m.shells, c); return nil }
func (m *mockProc) ShellEx(c string, flags []string) error {
	m.shellEx = append(m.shellEx, shellCap{cmd: c, flags: append([]string(nil), flags...)})
	return nil
}

type mockSearch struct {
	engine string
	q      string
}

func (m *mockSearch) Search(e, q string) error { m.engine = e; m.q = q; return nil }

func newCtx(svcs *cmdbar.Services) *cmdbar.MemoryContext {
	c := cmdbar.NewMemoryContext()
	c.Svcs = svcs
	return c
}

func newReg(t *testing.T) *cmdbar.Registry {
	t.Helper()
	r := cmdbar.NewRegistry()
	funcs.RegisterActions(r)
	return r
}

// --- tests ---

func TestAction_Open(t *testing.T) {
	mo := &mockOpen{}
	r := newReg(t)
	spec, _ := r.Lookup("open")
	ctx := newCtx(&cmdbar.Services{Open: mo})
	if _, err := spec.Eval(ctx, []string{"https://example.com"}); err != nil {
		t.Fatalf("open: %v", err)
	}
	if len(mo.got) != 1 || mo.got[0] != "https://example.com" {
		t.Errorf("Open got %v", mo.got)
	}
}

func TestAction_Open_ServiceMissing(t *testing.T) {
	r := newReg(t)
	spec, _ := r.Lookup("open")
	ctx := newCtx(&cmdbar.Services{}) // Open=nil
	_, err := spec.Eval(ctx, []string{"x"})
	if !errors.Is(err, cmdbar.ErrServiceUnavailable) {
		t.Errorf("want ErrServiceUnavailable, got %v", err)
	}
}

func TestAction_Open_NoServicesAtAll(t *testing.T) {
	r := newReg(t)
	spec, _ := r.Lookup("open")
	ctx := newCtx(nil)
	_, err := spec.Eval(ctx, []string{"x"})
	if !errors.Is(err, cmdbar.ErrServiceUnavailable) {
		t.Errorf("want ErrServiceUnavailable, got %v", err)
	}
}

func TestAction_Run(t *testing.T) {
	mp := &mockProc{}
	r := newReg(t)
	spec, _ := r.Lookup("proc.run")
	ctx := newCtx(&cmdbar.Services{Proc: mp})
	if _, err := spec.Eval(ctx, []string{"notepad", "a.txt"}); err != nil {
		t.Fatal(err)
	}
	if len(mp.runs) != 1 || mp.runs[0] != "notepad|a.txt" {
		t.Errorf("runs = %v", mp.runs)
	}
}

func TestAction_Shell(t *testing.T) {
	mp := &mockProc{}
	r := newReg(t)
	spec, _ := r.Lookup("proc.shell")
	ctx := newCtx(&cmdbar.Services{Proc: mp})
	if _, err := spec.Eval(ctx, []string{"echo hi"}); err != nil {
		t.Fatal(err)
	}
	if len(mp.shells) != 1 || mp.shells[0] != "echo hi" {
		t.Errorf("shells = %v", mp.shells)
	}
	if len(mp.shellEx) != 0 {
		t.Errorf("1-arg shell should not call ShellEx, got %v", mp.shellEx)
	}
}

// TestAction_ShellWithFlags 验证 shell(cmd, flags) 走 ShellEx 通路;
// flags 字符串按逗号拆分并去空白。
func TestAction_ShellWithFlags(t *testing.T) {
	cases := []struct {
		flagStr string
		want    []string
	}{
		{"term", []string{"term"}},
		{"pwsh", []string{"pwsh"}},
		{"pwsh,term", []string{"pwsh", "term"}},
		{" term , pwsh ", []string{"term", "pwsh"}},
		{"", nil}, // 空 flag 字符串等同省略
	}
	for _, c := range cases {
		mp := &mockProc{}
		r := newReg(t)
		spec, _ := r.Lookup("proc.shell")
		ctx := newCtx(&cmdbar.Services{Proc: mp})
		if _, err := spec.Eval(ctx, []string{"echo hi", c.flagStr}); err != nil {
			t.Fatalf("flag %q: %v", c.flagStr, err)
		}
		if len(mp.shellEx) != 1 {
			t.Fatalf("flag %q: want 1 ShellEx call, got %d", c.flagStr, len(mp.shellEx))
		}
		got := mp.shellEx[0]
		if got.cmd != "echo hi" {
			t.Errorf("flag %q: cmd = %q", c.flagStr, got.cmd)
		}
		if !sliceEq(got.flags, c.want) {
			t.Errorf("flag %q: flags = %v want %v", c.flagStr, got.flags, c.want)
		}
	}
}

func sliceEq(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

func TestAction_KeyTap_KeySeq(t *testing.T) {
	mk := &mockKeys{}
	r := newReg(t)
	ctx := newCtx(&cmdbar.Services{Keys: mk})
	tap, _ := r.Lookup("key.tap")
	if _, err := tap.Eval(ctx, []string{"Ctrl+C"}); err != nil {
		t.Fatal(err)
	}
	seq, _ := r.Lookup("key.seq")
	if _, err := seq.Eval(ctx, []string{"Home", "Shift+End", "Backspace"}); err != nil {
		t.Fatal(err)
	}
	if len(mk.taps) != 1 || mk.taps[0] != "Ctrl+C" {
		t.Errorf("taps = %v", mk.taps)
	}
	if len(mk.seqs) != 1 || len(mk.seqs[0]) != 3 || mk.seqs[0][1] != "Shift+End" {
		t.Errorf("seqs = %v", mk.seqs)
	}
}

func TestAction_ClipCopy_ClipPaste(t *testing.T) {
	mc := &mockClip{}
	mk := &mockKeys{}
	r := newReg(t)
	ctx := newCtx(&cmdbar.Services{Clip: mc, Keys: mk})
	cp, _ := r.Lookup("clip.copy")
	if _, err := cp.Eval(ctx, []string{"hello"}); err != nil {
		t.Fatal(err)
	}
	if mc.set != "hello" {
		t.Errorf("clip.copy set = %q", mc.set)
	}
	pp, _ := r.Lookup("clip.paste")
	if _, err := pp.Eval(ctx, nil); err != nil {
		t.Fatal(err)
	}
	if len(mk.taps) != 1 || mk.taps[0] != "Ctrl+V" {
		t.Errorf("clip.paste taps = %v", mk.taps)
	}
}

// 注: `type` 在 P5 之后由 eval.Evaluate 直接拦截为 ActionText, 不再走 registry
// EvalFunc 通路, 因此从 actionFuncs() 移除。type 的端到端验证由 eval 包负责
// (见 TestEvaluate_TypeBypassesRegistry / TestEvaluate_Action_DesignExample_JiaoBracket)。

func TestAction_Search_DefaultUsesOpen(t *testing.T) {
	mo := &mockOpen{}
	r := newReg(t)
	ctx := newCtx(&cmdbar.Services{Open: mo})
	sp, _ := r.Lookup("web.search")
	if _, err := sp.Eval(ctx, []string{"baidu", "你好 世界"}); err != nil {
		t.Fatal(err)
	}
	if len(mo.got) != 1 {
		t.Fatalf("Open got %v", mo.got)
	}
	want := "https://www.baidu.com/s?wd="
	if !strings.HasPrefix(mo.got[0], want) {
		t.Errorf("Open got %q, want prefix %q", mo.got[0], want)
	}
	if !strings.Contains(mo.got[0], "%E4%BD%A0%E5%A5%BD") { // "你好" url-encoded
		t.Errorf("Open got %q missing encoded query", mo.got[0])
	}
}

func TestAction_Search_UnknownEngine(t *testing.T) {
	mo := &mockOpen{}
	r := newReg(t)
	ctx := newCtx(&cmdbar.Services{Open: mo})
	sp, _ := r.Lookup("web.search")
	if _, err := sp.Eval(ctx, []string{"yahoo", "q"}); err == nil {
		t.Error("want error for unknown engine")
	}
}

func TestAction_Search_CustomEngine(t *testing.T) {
	ms := &mockSearch{}
	r := newReg(t)
	ctx := newCtx(&cmdbar.Services{Search: ms})
	sp, _ := r.Lookup("web.search")
	if _, err := sp.Eval(ctx, []string{"bing", "go"}); err != nil {
		t.Fatal(err)
	}
	if ms.engine != "bing" || ms.q != "go" {
		t.Errorf("mockSearch got engine=%q q=%q", ms.engine, ms.q)
	}
}

func TestAction_AllReturnEmptyDisplay(t *testing.T) {
	// All action functions must return empty string display because they
	// are side-effecting; callers should never embed their result in
	// display expressions.
	r := newReg(t)
	for _, name := range []string{"open", "run", "shell", "key.tap", "clip.copy"} {
		spec, _ := r.Lookup(name)
		if spec.Pure {
			t.Errorf("%s: want Pure=false", name)
		}
	}
}
