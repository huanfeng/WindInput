package dict

// value_expand.go — 任意候选 value 的统一展开器, 支持 `$CC(` 命令直通车
// 与模板变量 ($Y/$M/$WC/$uuid 等)。
//
// 设计目标 (见 P5 任务说明):
//   - PhraseLayer (短语) 与 coordinator 候选后处理 (码表/用户词库) 共用同一
//     展开逻辑, 避免多处复制粘贴。
//   - 大部分候选 value 不含 "$", 调用方应在调 Expand 之前用快路径过滤,
//     避免无谓的字符串扫描。

import (
	"strings"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/cmdbar"
)

// ValueExpander 把含 `$CC(` 或模板变量的 value 文本展开成 (text, display, actions)。
// Hook 为 nil 时 $CC 内容会被原样返回; TemplateEngine 为 nil 时也仅做 $CC 处理。
// ArrayHook 为 nil 时 $SS 内容回落到字面量候选。
type ValueExpander struct {
	Hook           CmdbarPhraseHook
	TemplateEngine *TemplateEngine
	ArrayHook      CmdbarArrayHook
}

// ExpandResult 是 ValueExpander.Expand 的返回三元组 + 元信息。
type ExpandResult struct {
	// Text 用于 candidate.Candidate.Text (上屏文本或 fallback 字面量)
	Text string
	// DisplayText 用于候选框显示, 空时调用方应回落到 Text
	DisplayText string
	// Actions 是选中后要执行的已解析动作链 (Effect + Text 混合)
	Actions []cmdbar.ResolvedAction
	// Modifiers 是 cmdbar marker 的 options bag 合并结果 (含 marker syntax
	// sugar 默认 + 显式 options); 仅在 IsCommand 为 true 且 hook 成功时填充。
	// 调用方透传到 candidate.Candidate.Modifiers, 替代旧 IsExactOnly 字符串扫描。
	Modifiers map[string]any
	// IsCommand 为 true 表示原 value 是命令 (含 $CC); 即便解析失败也保留 true,
	// 这样上层可标记 candidate.IsCommand=true 以走命令选中通路。
	IsCommand bool
	// Changed 为 true 表示展开后的 (Text/DisplayText/Actions) 与原 value 有差异,
	// 调用方可据此决定是否覆盖原候选字段; false 时应保留原状, 避免无谓写入。
	Changed bool
}

// HasExpandable 快速判断 value 是否值得交给 Expand 处理。
// 调用方建议先用 strings.IndexByte(text, '$') 做最便宜的早跳, 再调本函数。
func HasExpandable(value string) bool {
	if strings.IndexByte(value, '$') < 0 {
		return false
	}
	return HasCmdbarMarker(value) || HasVariable(value)
}

// Expand 按以下优先级处理 value:
//
//  1. 含 cmdbar marker (`$CC(` 或 `$CC1(`) 且 Hook 非 nil → 调 hook
//     - err: 字面量降级, IsCommand=true, Actions=nil
//     - ok=true: 完整三元组, IsCommand=true
//     - ok=false: 走步骤 2 (hook 主动放弃)
//  2. 含模板变量 ($X 但不是 $CC) 且 TemplateEngine 非 nil → templateEngine.Expand
//  3. 都不含或对应组件未注入 → 原样返回 (Changed=false)
//
// hook 抛错时本函数不写日志, 由调用方按上下文决定 (PhraseLayer 自己记 WARN,
// coordinator 候选后处理可以选择忽略, 因为大量候选不希望污染日志)。
func (ve *ValueExpander) Expand(value string) ExpandResult {
	if HasCmdbarMarker(value) && ve.Hook != nil {
		display, actions, modifiers, ok, err := ve.Hook(value)
		if err != nil {
			return ExpandResult{
				Text:      value,
				IsCommand: true,
				Changed:   false, // 字面量与原 value 等价, 调用方无需覆盖
			}
		}
		if ok {
			text := display
			if text == "" {
				text = value
			}
			return ExpandResult{
				Text:        text,
				DisplayText: display,
				Actions:     actions,
				Modifiers:   modifiers,
				IsCommand:   true,
				Changed:     true,
			}
		}
		// ok=false: hook 不处理这个 value, 走模板路径
	}
	if HasVariable(value) && ve.TemplateEngine != nil {
		expanded := ve.TemplateEngine.Expand(value)
		if expanded == value {
			return ExpandResult{Text: value, Changed: false}
		}
		return ExpandResult{Text: expanded, Changed: true}
	}
	return ExpandResult{Text: value, Changed: false}
}

// ExpandToCandidates 把一个 raw value 展开成一条或多条候选（value 式，供特殊码表等场景用）。
//
//   - $AA → ParseAAMarker 逐字符 N 条; 解析失败 → 1 条字面量
//   - $SS → ArrayHook N 条; hook 为 nil 或失败 → 1 条字面量
//   - $CC/$X → Expand 1 条
//   - 其它 → 1 条字面量
func (ve *ValueExpander) ExpandToCandidates(code, value string) []candidate.Candidate {
	// $AA 字符组: 现解析 marker → 逐 rune 候选
	if HasAAMarker(value) {
		if name, chars, ok := ParseAAMarker(value); ok {
			out := make([]candidate.Candidate, 0, len([]rune(chars)))
			for _, r := range chars {
				out = append(out, candidate.Candidate{Text: string(r), Code: code, Comment: name})
			}
			if len(out) > 0 {
				return out
			}
		}
		// ok=false 或空: 落到字面量
		return []candidate.Candidate{{Text: value, Code: code}}
	}
	// $SS 字符串数组: 运行时 hook
	if HasSSMarker(value) && ve.ArrayHook != nil {
		if name, elements, _, ok, err := ve.ArrayHook(value); ok && err == nil {
			out := make([]candidate.Candidate, 0, len(elements))
			for _, el := range elements {
				out = append(out, candidate.Candidate{Text: el.Display, Code: code, Comment: name, Actions: el.Actions})
			}
			if len(out) > 0 {
				return out
			}
		}
		return []candidate.Candidate{{Text: value, Code: code}}
	}
	// $CC / $X / 纯文本: 单候选
	res := ve.Expand(value)
	text := res.Text
	if text == "" {
		text = value
	}
	return []candidate.Candidate{{Text: text, Code: code, Actions: res.Actions}}
}
