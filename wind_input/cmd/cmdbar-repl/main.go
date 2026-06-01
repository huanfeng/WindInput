// Command cmdbar-repl is a manual smoke-test harness for the
// command-bar (内部 internal/cmdbar) library. It wires real Windows
// services (clipboard, key injection, process launch) into the
// evaluator so a developer can verify side-effects end-to-end before
// the P4 coordinator integration lands.
//
// Usage:
//
//	cmdbar-repl                     # interactive REPL
//	cmdbar-repl -e '<phrase>'       # one-shot evaluation
//
// REPL commands (lines starting with ':'):
//
//	:set input <value>      set input() / tail() / sub() source
//	:set last <value>       push onto history; last(1) becomes <value>
//	:set clip <value>       write to the system clipboard (real)
//	:show                   print current input/last buffer/clip
//	:help                   list commands
//	:quit                   exit
//
// Any other line is evaluated as a phrase value. Display is printed,
// then action thunks are executed against real services.
package main

import (
	"bufio"
	"flag"
	"fmt"
	"os"
	"strconv"
	"strings"

	"github.com/huanfeng/wind_input/internal/clipboard"
	"github.com/huanfeng/wind_input/internal/cmdbar"
	"github.com/huanfeng/wind_input/internal/cmdbar/ast"
	"github.com/huanfeng/wind_input/internal/cmdbar/eval"
	"github.com/huanfeng/wind_input/internal/cmdbar/funcs"
	"github.com/huanfeng/wind_input/internal/cmdbar/parser"
	"github.com/huanfeng/wind_input/internal/keyinject"
	"github.com/huanfeng/wind_input/internal/proc"
)

// Real-service adapters. Each one is a thin wrapper that satisfies the
// matching cmdbar interface.

type clipSvc struct{}

func (clipSvc) SetText(s string) error   { return clipboard.SetText(s) }
func (clipSvc) GetText() (string, error) { return clipboard.GetText() }
func (clipSvc) Paste() error {
	c, err := keyinject.Parse("Ctrl+V")
	if err != nil {
		return err
	}
	return keyinject.Tap(c)
}

type keysSvc struct{}

func (keysSvc) Tap(combo string) error {
	c, err := keyinject.Parse(combo)
	if err != nil {
		return err
	}
	return keyinject.Tap(c)
}

func (keysSvc) Sequence(combos ...string) error {
	cs := make([]keyinject.Combo, 0, len(combos))
	for _, s := range combos {
		c, err := keyinject.Parse(s)
		if err != nil {
			return err
		}
		cs = append(cs, c)
	}
	return keyinject.Sequence(cs...)
}

func (keysSvc) Hold(combo string) error {
	c, err := keyinject.Parse(combo)
	if err != nil {
		return err
	}
	return keyinject.Hold(c)
}

func (keysSvc) Release(combo string) error {
	c, err := keyinject.Parse(combo)
	if err != nil {
		return err
	}
	return keyinject.Release(c)
}

func (keysSvc) TypeText(text string) error { return keyinject.TypeText(text) }

type openSvc struct{}

func (openSvc) Open(target string) error { return proc.Open(target) }

type procSvc struct{}

func (procSvc) Run(cmd string, args ...string) error { return proc.Run(cmd, args...) }
func (procSvc) Shell(cmdline string) error           { return proc.Shell(cmdline) }
func (procSvc) ShellEx(cmdline string, flags []string) error {
	return proc.ShellEx(cmdline, flags)
}

func main() {
	expr := flag.String("e", "", "evaluate a single phrase and exit")
	flag.Parse()

	// Wire real Windows services into the default registry.
	funcs.RegisterActions(cmdbar.DefaultRegistry)
	ctx := &cmdbar.MemoryContext{
		Svcs: &cmdbar.Services{
			Clip: clipSvc{},
			Keys: keysSvc{},
			Open: openSvc{},
			Proc: procSvc{},
			// Dict / IME / Search 留 nil, 触发时返回 ErrServiceUnavailable.
		},
		History: cmdbar.NewHistory(16),
	}

	if *expr != "" {
		evaluate(ctx, *expr)
		return
	}

	fmt.Println("cmdbar-repl — manual test harness for command-bar")
	fmt.Println("type ':help' for commands, or any phrase to evaluate.")
	sc := bufio.NewScanner(os.Stdin)
	prompt(ctx)
	for sc.Scan() {
		line := strings.TrimRight(sc.Text(), "\r\n")
		if line == "" {
			prompt(ctx)
			continue
		}
		if strings.HasPrefix(line, ":") {
			if !handleCommand(ctx, line) {
				return
			}
		} else {
			evaluate(ctx, line)
		}
		prompt(ctx)
	}
}

func prompt(ctx *cmdbar.MemoryContext) {
	fmt.Printf("[input=%q last=%q] > ", ctx.InputStr, ctx.History.Get(1))
}

func handleCommand(ctx *cmdbar.MemoryContext, line string) bool {
	parts := strings.SplitN(line, " ", 3)
	switch parts[0] {
	case ":quit", ":exit", ":q":
		return false
	case ":help", ":?":
		fmt.Println(":set input <v>  | :set last <v>  | :set clip <v>")
		fmt.Println(":select <n>     fire actions of element n from last $SS expansion (1-based)")
		fmt.Println(":show           | :quit")
		fmt.Println("any non-':' line is treated as a phrase and evaluated.")
	case ":set":
		if len(parts) < 3 {
			fmt.Println("usage: :set <input|last|clip> <value>")
			return true
		}
		switch parts[1] {
		case "input":
			ctx.InputStr = parts[2]

		case "last":
			ctx.History.Push(parts[2])
		case "clip":
			if err := clipboard.SetText(parts[2]); err != nil {
				fmt.Println("clipboard error:", err)
			}
		default:
			fmt.Println("unknown :set target:", parts[1])
		}
	case ":show":
		clip, _ := clipboard.GetText()
		fmt.Printf("input=%q\nlast(1)=%q\nclip=%q\n", ctx.InputStr, ctx.History.Get(1), clip)
	case ":select":
		if len(parts) < 2 {
			fmt.Println("usage: :select <n>")
			return true
		}
		n, err := strconv.Atoi(strings.TrimSpace(parts[1]))
		if err != nil || n < 1 || n > len(lastArrayElements) {
			fmt.Printf("invalid index %q (have %d elements)\n", parts[1], len(lastArrayElements))
			return true
		}
		el := lastArrayElements[n-1]
		fmt.Printf("firing element [%d]: %s\n", n, el.Display)
		runActions(el.Actions)
		// 模拟 IME 选词后 push history (与真实 PhraseLayer/coordinator 行为对齐)
		if el.Display != "" {
			ctx.History.Push(el.Display)
		}
	default:
		fmt.Println("unknown command:", parts[0])
	}
	return true
}

func evaluate(ctx *cmdbar.MemoryContext, src string) {
	ph, err := parser.Parse(src)
	if err != nil {
		fmt.Println("parse error:", err)
		return
	}
	// 2026-05-16: $SS ArrayPhrase 走 ExpandArray 展开为多元素列表, 每个元素
	// 单独打印 display + actions。其他 phrase 类型 (LiteralPhrase /
	// TemplatePhrase / CommandPhrase) 仍走单值 Evaluate 通路。
	if ap, ok := ph.(ast.ArrayPhrase); ok {
		evaluateArray(ctx, ap)
		return
	}
	display, actions, err := eval.Evaluate(ph, ctx, cmdbar.DefaultRegistry)
	if err != nil {
		fmt.Println("eval error:", err)
		return
	}
	fmt.Printf("display: %s\n", display)
	if cp, ok := ph.(ast.CommandPhrase); ok && len(cp.Modifiers) > 0 {
		fmt.Printf("modifiers: %v\n", cp.Modifiers)
	}
	runActions(actions)
}

// evaluateArray 把 $SS ArrayPhrase 展开成 N 个候选并依次打印; 用户可输入
// `:select <n>` 在 REPL 中选第 n 个候选并触发其 actions (类似真实 IME 选词)。
func evaluateArray(ctx *cmdbar.MemoryContext, ap ast.ArrayPhrase) {
	name, elements, modifiers, err := eval.ExpandArray(ap, ctx, cmdbar.DefaultRegistry)
	if err != nil {
		fmt.Println("expand error:", err)
		return
	}
	fmt.Printf("$SS group: %q  (%d elements)\n", name, len(elements))
	if len(modifiers) > 0 {
		fmt.Printf("group modifiers: %v\n", modifiers)
	}
	for i, e := range elements {
		actionsTag := ""
		if len(e.Actions) > 0 {
			actionsTag = fmt.Sprintf("  [%d actions]", len(e.Actions))
		}
		fmt.Printf("  [%d] %s%s\n", i+1, e.Display, actionsTag)
	}
	fmt.Println("(type ':select <n>' to fire actions of element n, or another phrase to continue)")
	lastArrayElements = elements // 让 :select 命令能引用
}

// lastArrayElements 缓存最近一次 $SS 展开结果, 供 :select 命令使用。
var lastArrayElements []eval.ArrayElement

// runActions 按顺序执行 ResolvedAction 链, ActionText 收集为 "committed" 文本。
func runActions(actions []cmdbar.ResolvedAction) {
	var committed strings.Builder
	for i, a := range actions {
		switch a.Kind {
		case cmdbar.ActionText:
			txt, err := a.Run()
			if err != nil {
				fmt.Printf("action[%d] text error: %v\n", i, err)
				continue
			}
			committed.WriteString(txt)
			fmt.Printf("action[%d] text: %q\n", i, txt)
		case cmdbar.ActionEffect:
			if _, err := a.Run(); err != nil {
				fmt.Printf("action[%d] effect error: %v\n", i, err)
			} else {
				fmt.Printf("action[%d] effect ok\n", i)
			}
		}
	}
	if committed.Len() > 0 {
		fmt.Printf("committed: %s\n", committed.String())
	}
}
