// Package funcs registers the built-in command-bar functions. Each
// registration kind lives in its own file. See
// docs/design/command-bar-design.md §3.
package funcs

import (
	"fmt"
	"regexp"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/huanfeng/wind_input/internal/cmdbar"
)

// §3.1 value functions.

// §3.1 取值函数. 它们 Pure=true (可在 $CC display 中使用), 但都依赖
// EvalContext 外部状态 (input/history/clipboard/time/foreground), 所以
// Deterministic=false (同一表达式两次求值结果可能不同, 缓存优化时要看
// Deterministic 而不是 Pure)。
func valueFuncs() []cmdbar.FuncSpec {
	v := cmdbar.CategoryValue
	return []cmdbar.FuncSpec{
		{Name: "code", Category: v, MinArgs: 0, MaxArgs: 1, Pure: true, Deterministic: false,
			Description: "触发候选时的输入编码; code(n) 从第 n 字符 (1 起) 切到末尾",
			ExampleSrc:  `code()`, Eval: fnCode},
		{Name: "tail", Category: v, MinArgs: 2, MaxArgs: 2, Pure: true, Deterministic: true,
			Description: "字符串 s 从第 n 字符 (1 起) 切到末尾",
			ExampleSrc:  `tail(code, 2)`, Eval: fnTail},
		{Name: "last", Category: v, MinArgs: 0, MaxArgs: 1, Pure: true, Deterministic: false,
			Description: "最近一次上屏文本; last(n) 取倒数第 n 次, n≥1",
			ExampleSrc:  `last()`, Eval: fnLast},
		{Name: "clip", Category: v, MinArgs: 0, MaxArgs: 1, Pure: true, Deterministic: false,
			Description: "当前剪贴板内容; clip(n) 取历史第 n 条 (1-based, 容量 9)",
			ExampleSrc:  `clip()`, Eval: fnClip},
		{Name: "sel", Category: v, MinArgs: 0, MaxArgs: 0, Pure: true, Deterministic: false,
			Description: "当前前台应用中选中的文本 (无选区返回空, 当前为占位实现)",
			ExampleSrc:  `sel()`, Eval: fnSel},
		{Name: "app", Category: v, MinArgs: 0, MaxArgs: 0, Pure: true, Deterministic: false,
			Description: "当前前台进程名 (basename)",
			ExampleSrc:  `app()`, Eval: fnApp},
		{Name: "title", Category: v, MinArgs: 0, MaxArgs: 0, Pure: true, Deterministic: false,
			Description: "当前前台窗口标题",
			ExampleSrc:  `title()`, Eval: fnTitle},
		{Name: "date", Category: v, MinArgs: 1, MaxArgs: 2, Pure: true, Deterministic: false,
			Description: "日期; fmt 用 YYYY MM DD HH mm ss; offset 形如 '+1d' '-2w' '+3M' '-1y'",
			ExampleSrc:  `date("YYYY-MM-DD", "+1d")`, Eval: fnDate},
		{Name: "time", Category: v, MinArgs: 0, MaxArgs: 1, Pure: true, Deterministic: false,
			Description: "当前时间; 默认 fmt='HH:mm:ss'",
			ExampleSrc:  `time("HH:mm")`, Eval: fnTime},
		{Name: "now", Category: v, MinArgs: 0, MaxArgs: 0, Pure: true, Deterministic: false,
			Description: "当前日期时间, 等价 date('YYYY-MM-DD HH:mm:ss')",
			ExampleSrc:  `now()`, Eval: fnNow},
		{Name: "env", Category: v, MinArgs: 1, MaxArgs: 1, Pure: true, Deterministic: false,
			Description: "读取环境变量",
			ExampleSrc:  `env("HOME")`, Eval: fnEnv},
	}
}

// runeSlice returns a slice of runes for 1-based indexing operations.
func runeSlice(s string) []rune { return []rune(s) }

// runeTailFrom returns the substring of s starting from the n-th rune
// (1-based). If n > len, returns "". If n <= 0, treated as 1.
func runeTailFrom(s string, n int) string {
	if n <= 1 {
		return s
	}
	rs := runeSlice(s)
	if n-1 >= len(rs) {
		return ""
	}
	return string(rs[n-1:])
}

func parseArgInt(s string) (int, error) {
	s = strings.TrimSpace(s)
	if s == "" {
		return 0, fmt.Errorf("expected integer, got empty string")
	}
	// Allow floats whose value is integral.
	if i, err := strconv.Atoi(s); err == nil {
		return i, nil
	}
	f, err := strconv.ParseFloat(s, 64)
	if err != nil {
		return 0, err
	}
	return int(f), nil
}

// fnCode 返回触发候选时的输入编码 (inputBuffer 快照)。
// 旧名 "input" 在 P5 改为 "code", 避免和"输入"语义混淆;
// hook 进入时在 evalCtx.input 中冻结一份快照, 异步 action 内仍能拿到
// 当时的编码 (此时 coordinator.inputBuffer 已被 clearState 清空)。
func fnCode(ctx cmdbar.EvalContext, args []string) (string, error) {
	in := ctx.Input()
	if len(args) == 0 {
		return in, nil
	}
	n, err := parseArgInt(args[0])
	if err != nil {
		return "", fmt.Errorf("code: %w", err)
	}
	return runeTailFrom(in, n), nil
}

func fnTail(ctx cmdbar.EvalContext, args []string) (string, error) {
	n, err := parseArgInt(args[1])
	if err != nil {
		return "", fmt.Errorf("tail: %w", err)
	}
	return runeTailFrom(args[0], n), nil
}

func fnLast(ctx cmdbar.EvalContext, args []string) (string, error) {
	n := 1
	if len(args) == 1 {
		v, err := parseArgInt(args[0])
		if err != nil {
			return "", fmt.Errorf("last: %w", err)
		}
		n = v
	}
	if n < 1 {
		return "", nil
	}
	return ctx.Last(n), nil
}

func fnClip(ctx cmdbar.EvalContext, args []string) (string, error) {
	n := 0
	if len(args) == 1 {
		v, err := parseArgInt(args[0])
		if err != nil {
			return "", fmt.Errorf("clip: %w", err)
		}
		n = v
	}
	return ctx.Clip(n), nil
}

func fnSel(ctx cmdbar.EvalContext, args []string) (string, error)   { return ctx.Sel(), nil }
func fnApp(ctx cmdbar.EvalContext, args []string) (string, error)   { return ctx.App(), nil }
func fnTitle(ctx cmdbar.EvalContext, args []string) (string, error) { return ctx.Title(), nil }

// fmtAliases maps the user-facing date alias tokens to Go's reference
// time layout fragments. Order matters: longer aliases must be
// processed before their prefixes (e.g. YYYY before YY).
var fmtAliases = []struct {
	From, To string
}{
	{"YYYY", "2006"},
	{"YY", "06"},
	{"MM", "01"},
	{"DD", "02"},
	{"HH", "15"},
	{"mm", "04"},
	{"ss", "05"},
	{"M", "1"},
	{"D", "2"},
	{"h", "3"},
	{"m", "4"},
	{"s", "5"},
}

// translateFmt rewrites a user format string ("YYYY-MM-DD HH:mm") into
// the Go time-layout syntax. Tokens inside placeholder regions are
// replaced once each, longest-prefix-first.
func translateFmt(in string) string {
	// We walk the input character-by-character. To prevent later token
	// passes from re-substituting earlier outputs (e.g. "06" generated
	// for "YY" being re-scanned as something else), we emit into a
	// builder while consuming runs from the input.
	var b strings.Builder
	i := 0
	for i < len(in) {
		matched := false
		for _, a := range fmtAliases {
			if strings.HasPrefix(in[i:], a.From) {
				b.WriteString(a.To)
				i += len(a.From)
				matched = true
				break
			}
		}
		if !matched {
			b.WriteByte(in[i])
			i++
		}
	}
	return b.String()
}

var offsetRE = regexp.MustCompile(`^([+-]\d+)([dwMy])$`)

// applyOffset shifts t by the encoded offset string. Supported units:
// "d" days, "w" weeks (7 days), "M" months, "y" years. Empty offsets
// return t unchanged.
func applyOffset(t time.Time, offset string) (time.Time, error) {
	if offset == "" {
		return t, nil
	}
	m := offsetRE.FindStringSubmatch(offset)
	if m == nil {
		return t, fmt.Errorf("invalid offset %q (want e.g. +1d / -2w / +3M / -1y)", offset)
	}
	n, err := strconv.Atoi(m[1])
	if err != nil {
		return t, err
	}
	switch m[2] {
	case "d":
		return t.AddDate(0, 0, n), nil
	case "w":
		return t.AddDate(0, 0, n*7), nil
	case "M":
		return t.AddDate(0, n, 0), nil
	case "y":
		return t.AddDate(n, 0, 0), nil
	}
	return t, fmt.Errorf("invalid offset unit %q", m[2])
}

func fnDate(ctx cmdbar.EvalContext, args []string) (string, error) {
	t := ctx.Now()
	if len(args) == 2 {
		nt, err := applyOffset(t, args[1])
		if err != nil {
			return "", fmt.Errorf("date: %w", err)
		}
		t = nt
	}
	layout := translateFmt(args[0])
	return t.Format(layout), nil
}

func fnTime(ctx cmdbar.EvalContext, args []string) (string, error) {
	fmtStr := "HH:mm:ss"
	if len(args) == 1 {
		fmtStr = args[0]
	}
	return ctx.Now().Format(translateFmt(fmtStr)), nil
}

func fnNow(ctx cmdbar.EvalContext, args []string) (string, error) {
	return ctx.Now().Format(translateFmt("YYYY-MM-DD HH:mm:ss")), nil
}

func fnEnv(ctx cmdbar.EvalContext, args []string) (string, error) {
	return ctx.Env(args[0]), nil
}

// utf8Len returns the rune count of s without allocating.
func utf8Len(s string) int { return utf8.RuneCountInString(s) }
