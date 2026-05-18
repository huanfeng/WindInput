package dict

import (
	"testing"

	"github.com/huanfeng/wind_input/internal/candidate"
)

// TestApplyShadowPins_MatchByWord 验证旧行为: rule.CandID 为空 → 按 word 匹配 Text。
func TestApplyShadowPins_MatchByWord(t *testing.T) {
	cands := []candidate.Candidate{
		{Text: "你好", Code: "nh"},
		{Text: "你好啊", Code: "nh"},
		{Text: "您好", Code: "nh"},
	}
	rules := &ShadowRules{
		Pinned: []PinnedWord{{Word: "您好", Position: 0}},
	}
	out := ApplyShadowPins(cands, rules)
	if out[0].Text != "您好" {
		t.Fatalf("expected '您好' at position 0, got %q", out[0].Text)
	}
}

// TestApplyShadowPins_MatchByCandID 验证 R2 新行为: rule.CandID 非空 → 按 cand.ID 匹配。
// 关键场景: 两个 cand 的 Text 相同 (动态短语跨日子展开), 但 ID 不同, 应只 pin 命中的那条。
func TestApplyShadowPins_MatchByCandID(t *testing.T) {
	cands := []candidate.Candidate{
		{Text: "2026-05-17", Code: "rq", ID: "phrase:rq:$Y-$MM-$DD"},
		{Text: "2026-05-17", Code: "rq", ID: "phrase:rq:$Y年$M月$D日"}, // 同 Text 不同 ID
		{Text: "今天", Code: "rq", ID: "phrase:rq:今天"},
	}
	rules := &ShadowRules{
		Pinned: []PinnedWord{
			{Word: "ignored-on-id-match", CandID: "phrase:rq:$Y年$M月$D日", Position: 0},
		},
	}
	out := ApplyShadowPins(cands, rules)
	if out[0].ID != "phrase:rq:$Y年$M月$D日" {
		t.Fatalf("expected the $Y年$M月$D日 candidate at position 0, got id=%q", out[0].ID)
	}
}

// TestApplyShadowPins_IDFallsBackWhenAbsent 验证: rule 有 CandID 但 cand 没填 ID 时不命中,
// 不会误删/误 pin 其他候选 (rule 失效, 等价于"短语模板已变更")。
func TestApplyShadowPins_IDFallsBackWhenAbsent(t *testing.T) {
	cands := []candidate.Candidate{
		{Text: "测试", Code: "cs"}, // 普通词条, 无 ID
	}
	rules := &ShadowRules{
		Pinned: []PinnedWord{{Word: "测试", CandID: "phrase:cs:stale", Position: 0}},
	}
	out := ApplyShadowPins(cands, rules)
	// rule.CandID 非空时严格按 id 匹配, cand.ID 为空 → 不匹配 → 候选保持原样
	if len(out) != 1 || out[0].Text != "测试" {
		t.Fatalf("expected 1 candidate unchanged, got %+v", out)
	}
}

// TestApplyShadowPins_DeleteByID 验证按 candID 隐藏候选。
func TestApplyShadowPins_DeleteByID(t *testing.T) {
	cands := []candidate.Candidate{
		{Text: "短语 A", Code: "p", ID: "phrase:p:A"},
		{Text: "短语 B", Code: "p", ID: "phrase:p:B"},
	}
	rules := &ShadowRules{
		Deleted: []DeletedWord{{Word: "短语 A", CandID: "phrase:p:A"}},
	}
	out := ApplyShadowPins(cands, rules)
	if len(out) != 1 {
		t.Fatalf("expected 1 candidate after delete, got %d", len(out))
	}
	if out[0].ID != "phrase:p:B" {
		t.Fatalf("expected B to remain, got id=%q", out[0].ID)
	}
}

// TestApplyShadowPins_SingleCharProtectedByWordOnly 验证单字保护仅在按 word 匹配时生效:
// 短语 / 命令候选用 candID 时可以"删除单字" (用户主动挑了具体单字)。
func TestApplyShadowPins_SingleCharProtectedByWordOnly(t *testing.T) {
	cands := []candidate.Candidate{
		{Text: "你", Code: "n"},
		{Text: "中", Code: "n", ID: "phrase:n:中"},
	}
	rules := &ShadowRules{
		Deleted: []DeletedWord{
			{Word: "你"},                       // 按 word 匹配, 单字应被忽略
			{Word: "中", CandID: "phrase:n:中"}, // 按 id 匹配, 单字可删
		},
	}
	out := ApplyShadowPins(cands, rules)
	if len(out) != 1 || out[0].Text != "你" {
		t.Fatalf("expected only 你 remaining (single-char protected by word match), got %+v", out)
	}
}
