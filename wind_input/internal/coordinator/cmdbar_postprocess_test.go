package coordinator

import (
	"strings"
	"testing"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/cmdbar"
	"github.com/huanfeng/wind_input/internal/dict"
)

// TestApplyValueExpansion 校验候选后处理对三类候选的分流:
//
//  1. 含 "$CC(" → hook 命中, Actions + DisplayText 已挂
//  2. 含模板变量 ($Y 等) → templateEngine 展开 Text
//  3. 不含 '$' (普通候选) → 早跳, 字段完全不变
//
// 不构造完整 Coordinator, 仅装配 ValueExpander + 调单方法。
func TestApplyValueExpansion(t *testing.T) {
	called := 0
	hook := dict.CmdbarPhraseHook(func(value string) (string, []cmdbar.ResolvedAction, map[string]any, bool, error) {
		called++
		return "打开百度", []cmdbar.ResolvedAction{
			{Kind: cmdbar.ActionEffect, Run: func() (string, error) { return "", nil }},
		}, nil, true, nil
	})
	c := &Coordinator{
		cmdbarValueExpander: &dict.ValueExpander{
			Hook:           hook,
			TemplateEngine: dict.GetTemplateEngine(),
		},
	}

	// case 1: $CC 命令候选 (来自码表/用户词库, PhraseTemplate 为空)
	cmd := &candidate.Candidate{
		Text: `$CC("打开百度", open("https://baidu.com"))`,
	}
	c.applyValueExpansion(cmd)
	if !cmd.IsCommand {
		t.Fatalf("$CC 候选应被标记 IsCommand=true, got %+v", cmd)
	}
	if cmd.DisplayText != "打开百度" {
		t.Fatalf("$CC 候选 DisplayText=%q want 打开百度", cmd.DisplayText)
	}
	if len(cmd.Actions) != 1 {
		t.Fatalf("$CC 候选 Actions 应为 1, got %d", len(cmd.Actions))
	}
	if called != 1 {
		t.Fatalf("hook 应被调用 1 次, got %d", called)
	}

	// case 2: $Y 模板变量, 应展开但 IsCommand 不被置位
	tmpl := &candidate.Candidate{Text: "$Y-$MM-$DD"}
	c.applyValueExpansion(tmpl)
	if tmpl.IsCommand {
		t.Errorf("$Y 模板候选不应被标记 IsCommand")
	}
	if strings.Contains(tmpl.Text, "$") {
		t.Errorf("$Y 模板应被展开, 还含有 $: %q", tmpl.Text)
	}
	if called != 1 {
		t.Errorf("$Y 模板不应触发 hook, called=%d", called)
	}

	// case 3: 普通候选 (不含 '$'), 字段保持原样
	plain := &candidate.Candidate{Text: "你好", Weight: 100}
	c.applyValueExpansion(plain)
	if plain.Text != "你好" || plain.Weight != 100 || plain.DisplayText != "" || len(plain.Actions) != 0 {
		t.Errorf("普通候选不应被修改, got %+v", *plain)
	}

	// case 4: 已是 PhraseLayer 命令候选 (PhraseTemplate != ""), 跳过避免双重处理
	phrase := &candidate.Candidate{
		Text:           "已展开后的固定文本",
		PhraseTemplate: `$CC("打开百度", open("https://baidu.com"))`,
		IsCommand:      true,
		DisplayText:    "已展开后的固定文本",
	}
	beforeHook := called
	c.applyValueExpansion(phrase)
	if called != beforeHook {
		t.Errorf("PhraseLayer 候选不应再次触发 hook, called diff=%d", called-beforeHook)
	}
}

// TestApplyValueExpansion_NilExpander 验证 expander 未装配时是 no-op。
func TestApplyValueExpansion_NilExpander(t *testing.T) {
	c := &Coordinator{}
	original := `$CC("x", open("y"))`
	cand := &candidate.Candidate{Text: original}
	c.applyValueExpansion(cand)
	if cand.Text != original || cand.IsCommand || cand.DisplayText != "" || len(cand.Actions) != 0 {
		t.Fatalf("nil expander 应是 no-op, got %+v", *cand)
	}
}

// TestApplyValueExpansion_PreservesOriginalAsPhraseTemplate 验证展开后原 marker
// 被保存到 cand.PhraseTemplate, 让 handleCandidateDelete 能用原 marker 在源词库
// (user/temp dict) Remove 命中。bug 修复: user dict 存的 $CC 删除失败 (问题 3)。
// 详见 docs/design/candidate-actions.md §2.1。
func TestApplyValueExpansion_PreservesOriginalAsPhraseTemplate(t *testing.T) {
	hook := dict.CmdbarPhraseHook(func(value string) (string, []cmdbar.ResolvedAction, map[string]any, bool, error) {
		return "汉典 · X", []cmdbar.ResolvedAction{
			{Kind: cmdbar.ActionEffect, Run: func() (string, error) { return "", nil }},
		}, nil, true, nil
	})
	c := &Coordinator{
		cmdbarValueExpander: &dict.ValueExpander{
			Hook:           hook,
			TemplateEngine: dict.GetTemplateEngine(),
		},
	}

	originalMarker := `$CC("汉典 · {last(1)}", open("https://www.zdic.net/hans/{url(last(1))}"))`
	cand := &candidate.Candidate{
		Text: originalMarker,
		Meta: candidate.CandidateMeta{IsUserDict: true},
	}
	c.applyValueExpansion(cand)

	if cand.PhraseTemplate != originalMarker {
		t.Fatalf("展开后 PhraseTemplate 应保留原 marker, got %q, want %q",
			cand.PhraseTemplate, originalMarker)
	}
	if cand.Text == originalMarker {
		t.Fatalf("展开后 cand.Text 应被改写为显示文本, 仍是原 marker: %q", cand.Text)
	}
	if !cand.Meta.IsUserDict {
		t.Fatalf("Meta.IsUserDict 应被保留, got %+v", cand.Meta)
	}
}
