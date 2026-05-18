// Package eval walks parsed phrases against an EvalContext to produce
// the display string and a list of resolved actions. See
// docs/design/command-bar-design.md §3 and §5.
package eval

import (
	"fmt"
	"strconv"

	"github.com/huanfeng/wind_input/internal/cmdbar"
	"github.com/huanfeng/wind_input/internal/cmdbar/ast"
)

// ActionThunk 保留为旧 P2-P4 测试兼容的纯副作用闭包别名。新代码请用
// cmdbar.ResolvedAction; ActionThunk 等价于 ActionEffect 形态的 Run。
type ActionThunk func() error

// ArrayElement is the result of expanding one $SS element. Display is
// already evaluated (string lit element → 字面量, embedded $CC → 求值后
// 的 display 字符串). Actions 是该元素自己的动作链 (string lit 元素时为 nil)。
// ElementModifiers 是嵌入 $CC 的 modifiers (group 级 prefix 已在外层 ArrayPhrase
// 解析时禁用, 这里只剩 async / scope 等元素级 modifier)。
type ArrayElement struct {
	Display          string
	Actions          []cmdbar.ResolvedAction
	ElementModifiers map[string]any
}

// ExpandArray evaluates an ast.ArrayPhrase by expanding each element to
// an ArrayElement. String literal elements become a "上屏文本" candidate
// (Display=字面量, Actions=nil); embedded CommandPhrase elements become a
// "动作"候选 (Display=求值后字符串, Actions=该 CommandPhrase 的动作链)。
//
// 返回的 (name, elements, groupModifiers) 直接对应 ArrayPhrase 的字段;
// 调用方 (dict layer / coordinator) 据此生成 N 个 candidate.Candidate。
//
// 设计 docs/design/command-bar-followup.md §4.3 / §4.4.5。
func ExpandArray(phrase ast.ArrayPhrase, ctx cmdbar.EvalContext, reg *cmdbar.Registry) (name string, elements []ArrayElement, groupModifiers map[string]any, err error) {
	if reg == nil {
		reg = cmdbar.DefaultRegistry
	}
	out := make([]ArrayElement, 0, len(phrase.Elements))
	for i, elemExpr := range phrase.Elements {
		switch v := elemExpr.(type) {
		case ast.StringLit:
			disp, err := evalStringLit(v, ctx, reg)
			if err != nil {
				return "", nil, nil, fmt.Errorf("$SS element %d: %w", i+1, err)
			}
			out = append(out, ArrayElement{Display: disp})
		case ast.CommandPhrase:
			// 内层 CommandPhrase: 求 display + actions, 全套使用 Evaluate
			// 但要传入 CommandPhrase 本身, 不是 ArrayPhrase。
			disp, acts, err := Evaluate(v, ctx, reg)
			if err != nil {
				return "", nil, nil, fmt.Errorf("$SS element %d: %w", i+1, err)
			}
			out = append(out, ArrayElement{
				Display:          disp,
				Actions:          acts,
				ElementModifiers: v.Modifiers,
			})
		default:
			return "", nil, nil, fmt.Errorf("$SS element %d: unsupported expr %T", i+1, elemExpr)
		}
	}
	return phrase.Name, out, phrase.Modifiers, nil
}

// Evaluate runs the phrase against ctx using reg as the function table.
// It returns the rendered display text and the ordered list of resolved
// actions. A nil reg defaults to cmdbar.DefaultRegistry.
//
// 对 CommandPhrase.Actions 的特殊处理: 若 action 是 `type(arg)` 调用,
// 则不走 registry 查找, 直接构造 ActionText (Run 时求值 arg 并把结果作为
// 上屏文本返回); 其它动作 (open / key.tap / dict.addword / ...) 走
// 通用路径, 构造 ActionEffect 调用 registry.Eval 产生副作用。
//
// 这样 `$CC("《》", type("《》"), key.tap("Left"))` 的语义在宿主侧表现为:
// 把 ActionText 拼接为 InsertText, 然后异步触发后续的 key.tap("Left")。
func Evaluate(phrase ast.Phrase, ctx cmdbar.EvalContext, reg *cmdbar.Registry) (string, []cmdbar.ResolvedAction, error) {
	if reg == nil {
		reg = cmdbar.DefaultRegistry
	}
	switch p := phrase.(type) {
	case ast.LiteralPhrase:
		return p.Text, nil, nil
	case ast.TemplatePhrase:
		s, err := evalExpr(p.Expr, ctx, reg)
		if err != nil {
			return "", nil, err
		}
		return s, nil, nil
	case ast.ArrayPhrase:
		return "", nil, fmt.Errorf("eval: ArrayPhrase must be expanded via ExpandArray, not Evaluate")
	case ast.CommandPhrase:
		if err := assertPureDisplay(p.Display, reg); err != nil {
			return "", nil, err
		}
		disp, err := evalExpr(p.Display, ctx, reg)
		if err != nil {
			return "", nil, err
		}
		actions := make([]cmdbar.ResolvedAction, 0, len(p.Actions))
		for _, act := range p.Actions {
			a := act // capture
			if call, ok := a.(ast.Call); ok && call.Name == "type" {
				// type(arg): 把参数求值为字符串, 由宿主走 InsertText 上屏。
				// arity 在此处显式校验 (1 参), 与 registry stub 对齐。
				if len(call.Args) != 1 {
					return "", nil, fmt.Errorf("type: expected 1 arg, got %d", len(call.Args))
				}
				argExpr := call.Args[0]
				actions = append(actions, cmdbar.ResolvedAction{
					Kind: cmdbar.ActionText,
					Run: func() (string, error) {
						return evalExpr(argExpr, ctx, reg)
					},
				})
				continue
			}
			actions = append(actions, cmdbar.ResolvedAction{
				Kind: cmdbar.ActionEffect,
				Run: func() (string, error) {
					_, err := evalExpr(a, ctx, reg)
					return "", err
				},
			})
		}
		return disp, actions, nil
	}
	return "", nil, fmt.Errorf("eval: unknown phrase type %T", phrase)
}

// assertPureDisplay walks expr and fails if any reference is to a
// non-pure function in reg. Bare identifiers and namespaced calls are
// both checked.
func assertPureDisplay(expr ast.Expr, reg *cmdbar.Registry) error {
	switch e := expr.(type) {
	case ast.StringLit:
		for _, part := range e.Parts {
			if ip, ok := part.(ast.InterpPart); ok {
				if err := assertPureDisplay(ip.Expr, reg); err != nil {
					return err
				}
			}
		}
		return nil
	case ast.NumberLit:
		return nil
	case ast.Ident:
		spec, ok := reg.Lookup(e.Name)
		if !ok {
			return fmt.Errorf("display: unknown function %q", e.Name)
		}
		if !spec.Pure {
			return fmt.Errorf("display: function %q is not allowed (side-effecting)", e.Name)
		}
		return nil
	case ast.Call:
		spec, ok := reg.Lookup(e.Name)
		if !ok {
			return fmt.Errorf("display: unknown function %q", e.Name)
		}
		if !spec.Pure {
			return fmt.Errorf("display: function %q is not allowed (side-effecting)", e.Name)
		}
		for _, a := range e.Args {
			if err := assertPureDisplay(a, reg); err != nil {
				return err
			}
		}
		return nil
	}
	return fmt.Errorf("display: unsupported expression %T", expr)
}

// evalExpr reduces an expression to its string value.
func evalExpr(expr ast.Expr, ctx cmdbar.EvalContext, reg *cmdbar.Registry) (string, error) {
	switch e := expr.(type) {
	case ast.StringLit:
		return evalStringLit(e, ctx, reg)
	case ast.NumberLit:
		if e.Raw != "" {
			return e.Raw, nil
		}
		return strconv.FormatFloat(e.Value, 'f', -1, 64), nil
	case ast.Ident:
		spec, ok := reg.Lookup(e.Name)
		if !ok {
			return "", fmt.Errorf("unknown function %q", e.Name)
		}
		if !spec.Accepts(0) {
			return "", fmt.Errorf("function %q does not accept zero arguments", e.Name)
		}
		return spec.Eval(ctx, nil)
	case ast.Call:
		spec, ok := reg.Lookup(e.Name)
		if !ok {
			return "", fmt.Errorf("unknown function %q", e.Name)
		}
		if !spec.Accepts(len(e.Args)) {
			return "", fmt.Errorf("function %q called with %d args (min=%d max=%d)",
				e.Name, len(e.Args), spec.MinArgs, spec.MaxArgs)
		}
		argVals := make([]string, 0, len(e.Args))
		for _, a := range e.Args {
			v, err := evalExpr(a, ctx, reg)
			if err != nil {
				return "", err
			}
			argVals = append(argVals, v)
		}
		return spec.Eval(ctx, argVals)
	}
	return "", fmt.Errorf("eval: unsupported expression %T", expr)
}

func evalStringLit(s ast.StringLit, ctx cmdbar.EvalContext, reg *cmdbar.Registry) (string, error) {
	var out []byte
	for _, part := range s.Parts {
		switch p := part.(type) {
		case ast.LiteralPart:
			out = append(out, p.Text...)
		case ast.InterpPart:
			v, err := evalExpr(p.Expr, ctx, reg)
			if err != nil {
				return "", err
			}
			out = append(out, v...)
		}
	}
	return string(out), nil
}
