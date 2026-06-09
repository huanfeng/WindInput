package coordinator

import (
	"testing"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/keys"
)

func TestMatchTriggerKeyInList(t *testing.T) {
	list := []string{"`", ";"}

	// 反引号：字面值匹配
	if got := matchTriggerKeyInList(list, "`", int(ipc.VK_OEM_3)); got != "`" {
		t.Errorf("grave by char: got %q, want `", got)
	}
	// 反引号：仅 VK 匹配（key 字段为空）
	if got := matchTriggerKeyInList(list, "", int(ipc.VK_OEM_3)); got != "`" {
		t.Errorf("grave by VK: got %q, want `", got)
	}
	// 分号
	if got := matchTriggerKeyInList(list, ";", int(ipc.VK_OEM_1)); got != ";" {
		t.Errorf("semicolon: got %q, want ;", got)
	}
	// 未配置的键
	if got := matchTriggerKeyInList(list, ".", int(ipc.VK_OEM_PERIOD)); got != "" {
		t.Errorf("period not in list: got %q, want empty", got)
	}
	// 空列表
	if got := matchTriggerKeyInList(nil, "`", int(ipc.VK_OEM_3)); got != "" {
		t.Errorf("nil list: got %q, want empty", got)
	}
}

func TestMatchQuickInputTrigger(t *testing.T) {
	cfg := &config.Config{}
	cfg.Input.QuickInput.TriggerKeys = []string{"`"}
	h := newTestCoordinator(t, withConfig(cfg))

	// enabled 且键匹配
	if got := h.matchQuickInputTrigger("`", int(ipc.VK_OEM_3)); got != "`" {
		t.Errorf("got %q, want `", got)
	}
	// 未配置触发键 → 不匹配
	empty := newTestCoordinator(t, withConfig(&config.Config{}))
	if got := empty.matchQuickInputTrigger("`", int(ipc.VK_OEM_3)); got != "" {
		t.Errorf("no trigger keys: got %q, want empty", got)
	}
}

func cand(text string) candidate.Candidate { return candidate.Candidate{Text: text} }

// ; 同时是二候选键(PairSemicolonQuote 第一键) 和 快捷输入触发键。
func newSemicolonDualCoordinator(t *testing.T, buffer string, cands ...candidate.Candidate) *testCoordinator {
	return newTestCoordinator(t,
		withSelectKeyGroups(keys.PairSemicolonQuote),
		withQuickInputTriggers(";"),
		withCandidates(buffer, cands...),
	)
}

func TestDecide_SelectKey2_WinsWhenEnoughCandidates(t *testing.T) {
	// 候选 ≥ 2：; 选第 2 候选（B 优先于 D）
	h := newSemicolonDualCoordinator(t, "ab", cand("啊"), cand("吧"))
	d := h.decideBufferedTrigger(";", int(ipc.VK_OEM_1))
	if d.kind != actSelectCandidate || d.candidateIdx != 1 {
		t.Fatalf("got kind=%v idx=%d, want actSelectCandidate idx=1", d.kind, d.candidateIdx)
	}
}

func TestDecide_FallbackToMode_WhenCandidatesInsufficient(t *testing.T) {
	// 只有 1 个候选：; 二候选无效 → 回落快捷输入（D），顶码上屏高亮候选(idx 0)
	h := newSemicolonDualCoordinator(t, "ab", cand("啊"))
	d := h.decideBufferedTrigger(";", int(ipc.VK_OEM_1))
	if d.kind != actEnterMode || d.modeName != "quick_input" || d.commitIdx != 0 {
		t.Fatalf("got %+v, want actEnterMode quick_input commitIdx=0", d)
	}
}

func TestDecide_PureModeKey_WithCandidates(t *testing.T) {
	// ` 不是选择键，纯模式键：有候选 → 进模式，顶码上屏高亮候选
	h := newTestCoordinator(t,
		withQuickInputTriggers("`"),
		withCandidates("ab", cand("啊"), cand("吧")),
	)
	d := h.decideBufferedTrigger("`", int(ipc.VK_OEM_3))
	if d.kind != actEnterMode || d.commitIdx != 0 {
		t.Fatalf("got %+v, want actEnterMode commitIdx=0", d)
	}
}

func TestDecide_EmptyCandidates_DiscardAndEnter(t *testing.T) {
	// buffer 非空但无候选（空码）：commitIdx=-1
	h := newTestCoordinator(t,
		withQuickInputTriggers("`"),
		withCandidates("zzz"), // 无候选
	)
	d := h.decideBufferedTrigger("`", int(ipc.VK_OEM_3))
	if d.kind != actEnterMode || d.commitIdx != -1 {
		t.Fatalf("got %+v, want actEnterMode commitIdx=-1", d)
	}
}

func TestDecide_GroupCandidate_FallsThroughToOverflow(t *testing.T) {
	// 高亮是组候选 + ; 是二候选键候选不足 → 不进模式，回落 overflow（与改动前一致）
	h := newSemicolonDualCoordinator(t, "ab", candidate.Candidate{Text: "组", IsGroup: true})
	d := h.decideBufferedTrigger(";", int(ipc.VK_OEM_1))
	if d.kind != actOverflow {
		t.Fatalf("got %+v, want actOverflow", d)
	}
}

func TestDecide_GroupCandidate_PureModeKey_FallsThroughToNone(t *testing.T) {
	// 高亮是组候选 + ` 纯模式键 → 不进模式，actNone（调用方走标点）
	h := newTestCoordinator(t,
		withQuickInputTriggers("`"),
		withCandidates("ab", candidate.Candidate{Text: "组", IsGroup: true}),
	)
	d := h.decideBufferedTrigger("`", int(ipc.VK_OEM_3))
	if d.kind != actNone {
		t.Fatalf("got %+v, want actNone", d)
	}
}

func TestDecide_NonRoleKey_None(t *testing.T) {
	// 句号既非选择键也非模式键 → actNone
	h := newTestCoordinator(t,
		withQuickInputTriggers("`"),
		withCandidates("ab", cand("啊"), cand("吧")),
	)
	d := h.decideBufferedTrigger(".", int(ipc.VK_OEM_PERIOD))
	if d.kind != actNone {
		t.Fatalf("got %+v, want actNone", d)
	}
}

func TestDecide_ModePriority_QuickInputBeatsTempPinyin(t *testing.T) {
	// ; 同时配给快捷输入；快捷输入优先级高于临时拼音，应命中 quick_input。
	// （临时拼音 enabled 依赖码表引擎，此处未挂引擎 → 仅快捷输入 enabled，
	//  仍能验证遍历顺序不会先命中后者。）
	h := newTestCoordinator(t,
		withQuickInputTriggers(";"),
		withSelectKeyGroups(keys.PairSemicolonQuote),
		withCandidates("ab", cand("啊")), // 1 个候选 → 二候选无效 → 进模式
	)
	d := h.decideBufferedTrigger(";", int(ipc.VK_OEM_1))
	if d.kind != actEnterMode || d.modeName != "quick_input" {
		t.Fatalf("got %+v, want actEnterMode quick_input", d)
	}
}

func TestMatchTempPinyin_EngineGate(t *testing.T) {
	// 码表引擎 + 临时拼音启用 + 配 ` 触发键 → 匹配
	h := newTestCoordinator(t, withZHybridSchema(false))
	h.config.Input.TempPinyin.TriggerKeys = append(h.config.Input.TempPinyin.TriggerKeys, "`")
	if got := h.matchTempPinyinTrigger("`", int(ipc.VK_OEM_3)); got != "`" {
		t.Errorf("codetable engine: got %q, want `", got)
	}
	// 无引擎 → 不匹配（引擎门禁）
	bare := newTestCoordinator(t)
	bare.config.Input.TempPinyin.TriggerKeys = []string{"`"}
	if got := bare.matchTempPinyinTrigger("`", int(ipc.VK_OEM_3)); got != "" {
		t.Errorf("no engine: got %q, want empty", got)
	}
}

func TestMatchTempPinyin_ExcludesZ(t *testing.T) {
	// z 配为临时拼音触发键，但 matchTempPinyinTrigger 不应匹配 z（z 走独立路径）
	h := newTestCoordinator(t, withZHybridSchema(false)) // 已含 "z" 触发键
	if got := h.matchTempPinyinTrigger("z", int('Z')); got != "" {
		t.Errorf("z should be excluded from punct trigger match: got %q", got)
	}
}
