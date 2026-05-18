// Package ast defines the abstract syntax tree node types used by the
// command bar parser. The grammar is documented in
// docs/design/command-bar-design.md §2.
package ast

import (
	"strconv"
	"strings"
)

// Expr is the common interface for all expression-level AST nodes.
type Expr interface {
	exprNode()
	String() string
}

// Part is a fragment inside a StringLit. It is either a LiteralPart with
// raw text or an InterpPart wrapping a nested expression.
type Part interface {
	partNode()
	String() string
}

// LiteralPart is a raw text segment of a string literal.
type LiteralPart struct {
	Text string
}

func (LiteralPart) partNode() {}
func (p LiteralPart) String() string {
	// Re-escape special characters for round-trippable debug output.
	r := strings.NewReplacer(
		`\`, `\\`,
		`"`, `\"`,
		`{`, `\{`,
		`}`, `\}`,
	)
	return r.Replace(p.Text)
}

// InterpPart is a `{expr}` interpolation segment inside a string literal.
type InterpPart struct {
	Expr Expr
}

func (InterpPart) partNode() {}
func (p InterpPart) String() string {
	if p.Expr == nil {
		return "{}"
	}
	return "{" + p.Expr.String() + "}"
}

// StringLit is a double-quoted string literal, optionally containing
// `{expr}` interpolations.
type StringLit struct {
	Parts []Part
}

func (StringLit) exprNode() {}
func (s StringLit) String() string {
	var b strings.Builder
	b.WriteByte('"')
	for _, p := range s.Parts {
		b.WriteString(p.String())
	}
	b.WriteByte('"')
	return b.String()
}

// NumberLit is a numeric literal. Raw keeps the original lexeme so we can
// emit it back verbatim; Value is the parsed float.
type NumberLit struct {
	Value float64
	Raw   string
}

func (NumberLit) exprNode() {}
func (n NumberLit) String() string {
	if n.Raw != "" {
		return n.Raw
	}
	return strconv.FormatFloat(n.Value, 'f', -1, 64)
}

// Ident is a bare identifier reference. Per §2.4 a bare identifier is
// semantically equivalent to a zero-argument call.
type Ident struct {
	Name string
}

func (Ident) exprNode()        {}
func (i Ident) String() string { return i.Name }

// Call is a function invocation. Name may contain a single dot for a
// namespace prefix (e.g. "clip.copy").
type Call struct {
	Name string
	Args []Expr
}

func (Call) exprNode() {}
func (c Call) String() string {
	var b strings.Builder
	b.WriteString(c.Name)
	b.WriteByte('(')
	for i, a := range c.Args {
		if i > 0 {
			b.WriteString(", ")
		}
		b.WriteString(a.String())
	}
	b.WriteByte(')')
	return b.String()
}

// Pair is one key:value entry inside an ObjectLit. Value is restricted to
// literal expressions (StringLit / NumberLit / Ident bool-or-name). The
// restriction is enforced by the parser, not by the type system.
//
// 设计 (docs/design/command-bar-followup.md §3.1):
//
//	pair = ident ":" value
//	value = string | number | "true" | "false" | ident
type Pair struct {
	Key   string
	Value Expr
}

// ObjectLit is a `{key: value, ...}` trailing options bag, used in marker
// call positions to carry modifier flags (prefix / expand / nav / async / ...).
// Per §3.1, ObjectLit is only permitted as the last argument of a call;
// the parser rejects mid-list ObjectLits.
//
// Pairs are kept in source order so consumers can preserve user intent
// when re-emitting; duplicate keys are tolerated by the parser but the
// last write wins when interpreted as a map.
type ObjectLit struct {
	Pairs []Pair
}

func (ObjectLit) exprNode() {}
func (o ObjectLit) String() string {
	var b strings.Builder
	b.WriteByte('{')
	for i, p := range o.Pairs {
		if i > 0 {
			b.WriteString(", ")
		}
		b.WriteString(p.Key)
		b.WriteString(": ")
		if p.Value != nil {
			b.WriteString(p.Value.String())
		}
	}
	b.WriteByte('}')
	return b.String()
}

// Phrase is a top-level AST node. A phrase is one of LiteralPhrase,
// TemplatePhrase or CommandPhrase.
type Phrase interface {
	phraseNode()
	String() string
}

// LiteralPhrase is a phrase with no interpolation and no `$CC(...)`
// wrapper. It surfaces as plain text.
type LiteralPhrase struct {
	Text string
}

func (LiteralPhrase) phraseNode()      {}
func (p LiteralPhrase) String() string { return p.Text }

// TemplatePhrase is a phrase containing `{expr}` interpolations but no
// `$CC(...)` wrapper.
type TemplatePhrase struct {
	Expr Expr // always a StringLit holding the template body
}

func (TemplatePhrase) phraseNode() {}
func (p TemplatePhrase) String() string {
	if p.Expr == nil {
		return ""
	}
	return p.Expr.String()
}

// CommandPhrase represents a `$CC(display, action...)` invocation.
// Display is the candidate display expression; Actions is the (possibly
// empty) ordered list of side-effecting action expressions.
//
// Modifiers carries the trailing options-bag values + marker syntax-sugar
// defaults (e.g. `$CC1(...)` injects `{prefix: true}` here at parse time);
// see docs/design/command-bar-followup.md §3.2 / §4.1. Nil when the
// caller wrote neither marker suffix nor options bag. Values are typed
// (string / float64 / bool) per the parser's literal-coercion rules.
type CommandPhrase struct {
	Display   Expr
	Actions   []Expr
	Modifiers map[string]any
}

func (CommandPhrase) phraseNode() {}

// exprNode lets CommandPhrase appear as an element inside ArrayPhrase
// (e.g. `$SS("name", $CC("d", open(...)), "literal")`). The parser only
// admits embedded CommandPhrases in ArrayPhrase argument position; bare
// `$CC(...)` is still parsed as a phrase via Parse().
func (CommandPhrase) exprNode() {}

func (p CommandPhrase) String() string {
	var b strings.Builder
	b.WriteString("$CC(")
	if p.Display != nil {
		b.WriteString(p.Display.String())
	}
	for _, a := range p.Actions {
		b.WriteString(", ")
		b.WriteString(a.String())
	}
	if len(p.Modifiers) > 0 {
		b.WriteString(", {")
		first := true
		// Note: map iteration order is randomized; debug callers should
		// canonicalize externally if they need stable output.
		for k, v := range p.Modifiers {
			if !first {
				b.WriteString(", ")
			}
			first = false
			b.WriteString(k)
			b.WriteString(": ")
			b.WriteString(formatModifierValue(v))
		}
		b.WriteByte('}')
	}
	b.WriteByte(')')
	return b.String()
}

// ArrayPhrase represents a `$SS("name", elem1, elem2, ..., {options})`
// invocation —— 字符串数组短语 (含命令嵌套)。
//
// 设计 (docs/design/command-bar-followup.md §4.3):
//   - Name 是第一个参数 string lit, 用作前缀导航候选的显示名
//   - Elements 是其余参数, 每个元素是 StringLit 或 embedded CommandPhrase
//     (字符串字面量当作"上屏文本"候选, 嵌入的 CommandPhrase 当作"动作"候选)
//   - Modifiers 同 CommandPhrase 路径, 含 marker 默认 + 显式 options 合并值
//   - 嵌套深度上限 1: ArrayPhrase 不能嵌入 ArrayPhrase, 内部 CommandPhrase
//     也不能再嵌入 marker (由 parser 在 parseArrayPhrase 内部检查)
type ArrayPhrase struct {
	Name      string
	Elements  []Expr
	Modifiers map[string]any
}

func (ArrayPhrase) phraseNode() {}
func (p ArrayPhrase) String() string {
	var b strings.Builder
	b.WriteString("$SS(")
	b.WriteByte('"')
	b.WriteString(p.Name)
	b.WriteByte('"')
	for _, e := range p.Elements {
		b.WriteString(", ")
		b.WriteString(e.String())
	}
	if len(p.Modifiers) > 0 {
		b.WriteString(", {")
		first := true
		for k, v := range p.Modifiers {
			if !first {
				b.WriteString(", ")
			}
			first = false
			b.WriteString(k)
			b.WriteString(": ")
			b.WriteString(formatModifierValue(v))
		}
		b.WriteByte('}')
	}
	b.WriteByte(')')
	return b.String()
}

// formatModifierValue renders a typed modifier value for round-trippable
// debug output. Kept private to ast since the modifier value space is
// intentionally narrow (string / float64 / bool only).
func formatModifierValue(v any) string {
	switch x := v.(type) {
	case string:
		return strconv.Quote(x)
	case bool:
		if x {
			return "true"
		}
		return "false"
	case float64:
		return strconv.FormatFloat(x, 'f', -1, 64)
	}
	return "<?>"
}
