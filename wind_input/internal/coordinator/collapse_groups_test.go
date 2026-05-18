package coordinator

import (
	"strings"
	"testing"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
)

// member 构造一个字符组 member 候选 (与 phrase.SearchCommand 出口的标记一致)。
// 同一 (groupCode, groupName) 共享 GroupTemplate, 让 collapse 按 GroupTemplate 分组时
// 把同组 member 算成一组。
func member(text, groupCode, groupName string, weight int) candidate.Candidate {
	return candidate.Candidate{
		Text:          text,
		Code:          groupCode,
		Weight:        weight,
		IsCommand:     true,
		IsPhrase:      true,
		IsGroupMember: true,
		GroupCode:     groupCode,
		GroupName:     groupName,
		GroupTemplate: `$AA("` + groupName + `", "X")`, // 同组共享占位 marker
	}
}

// other 构造一个非 group member 的普通候选 (码表 / 拼音 / 普通短语)。
func other(text string, weight int) candidate.Candidate {
	return candidate.Candidate{Text: text, Weight: weight}
}

// TestCollapseGroupMembersIfMixed_UniqueGroup 唯一 group 占据列表 → 保持展开。
func TestCollapseGroupMembersIfMixed_UniqueGroup(t *testing.T) {
	in := []candidate.Candidate{
		member("①", "zzsz", "圆数字", 3000),
		member("②", "zzsz", "圆数字", 3000),
		member("③", "zzsz", "圆数字", 3000),
	}
	out := collapseGroupMembersIfMixed(in, "")
	if len(out) != 3 {
		t.Fatalf("expected 3 candidates kept (unique group, no collapse), got %d", len(out))
	}
	for i, c := range out {
		if c.IsGroup {
			t.Fatalf("idx %d: should NOT be collapsed to nav, got IsGroup=true", i)
		}
		if !c.IsGroupMember {
			t.Fatalf("idx %d: should remain IsGroupMember=true", i)
		}
	}
}

// TestCollapseGroupMembersIfMixed_WithOtherCandidates 混合候选 → 字符组成员
// 全部 collapse 为 1 个 nav 候选, 其它候选原序保留。
func TestCollapseGroupMembersIfMixed_WithOtherCandidates(t *testing.T) {
	in := []candidate.Candidate{
		other("普通候选A", 100),
		member("①", "zzsz", "圆数字", 3000),
		member("②", "zzsz", "圆数字", 3000),
		member("③", "zzsz", "圆数字", 3000),
		other("普通候选B", 50),
	}
	out := collapseGroupMembersIfMixed(in, "")
	if len(out) != 3 {
		t.Fatalf("expected 3 candidates after collapse (2 others + 1 nav), got %d", len(out))
	}
	if out[0].Text != "普通候选A" {
		t.Fatalf("idx 0: want '普通候选A', got %q", out[0].Text)
	}
	// nav 在原 group 第一个 member 位置 (idx 1) 替换
	nav := out[1]
	if !nav.IsGroup {
		t.Fatalf("idx 1 should be nav (IsGroup=true), got %+v", nav)
	}
	if nav.GroupCode != "zzsz" {
		t.Fatalf("nav GroupCode want zzsz, got %q", nav.GroupCode)
	}
	if nav.Text != "圆数字" {
		t.Fatalf("nav Text want 圆数字, got %q", nav.Text)
	}
	if !strings.Contains(nav.Comment, "3") {
		t.Fatalf("nav Comment should describe member count, got %q", nav.Comment)
	}
	if nav.Weight != 3000 {
		t.Fatalf("nav Weight should inherit group member weight 3000, got %d", nav.Weight)
	}
	if !nav.IsPhrase {
		t.Fatalf("nav should preserve IsPhrase=true (phrase tier)")
	}
	if out[2].Text != "普通候选B" {
		t.Fatalf("idx 2: want '普通候选B', got %q", out[2].Text)
	}
}

// TestCollapseGroupMembersIfMixed_TwoGroups 两个 group 同时出现 →
// 各 collapse 为 1 个 nav (即使没有其它来源候选, 因为多 group 也算混合)。
func TestCollapseGroupMembersIfMixed_TwoGroups(t *testing.T) {
	in := []candidate.Candidate{
		member("①", "zzsz", "圆数字", 3000),
		member("②", "zzsz", "圆数字", 3000),
		member(",", "zzbd", "标点符号", 2500),
		member(".", "zzbd", "标点符号", 2500),
	}
	out := collapseGroupMembersIfMixed(in, "")
	if len(out) != 2 {
		t.Fatalf("expected 2 nav candidates after collapse, got %d", len(out))
	}
	codes := []string{}
	for _, c := range out {
		if !c.IsGroup {
			t.Fatalf("expected all to be nav, got %+v", c)
		}
		codes = append(codes, c.GroupCode)
	}
	// nav 顺序按各组首次出现位置
	if codes[0] != "zzsz" || codes[1] != "zzbd" {
		t.Fatalf("nav order want [zzsz zzbd], got %v", codes)
	}
}

// TestCollapseGroupMembersIfMixed_ExpandedGroupTemplate 用户主动选中 nav 进入二级展开:
// **仅保留该 group 的成员**, 过滤其它一切候选 ("此时展开, 只有这个数组自己")。
// 状态机 key 是 GroupTemplate (group 原 marker), 让同 code 多 group 也能独立追踪。
func TestCollapseGroupMembersIfMixed_ExpandedGroupTemplate(t *testing.T) {
	in := []candidate.Candidate{
		member("①", "zzsz", "圆数字", 3000),
		member("②", "zzsz", "圆数字", 3000),
		other("码表候选", 100),
	}
	zzszTpl := in[0].GroupTemplate
	// 用户已选过 zzsz 的 nav 进入二级 → 只保留该 group 成员, 过滤其它。
	out := collapseGroupMembersIfMixed(in, zzszTpl)
	if len(out) != 2 {
		t.Fatalf("expected 2 candidates (only zzsz members), got %d", len(out))
	}
	for i, c := range out {
		if !c.IsGroupMember || c.GroupCode != "zzsz" {
			t.Fatalf("idx %d: should be zzsz member, got %+v", i, c)
		}
	}

	// expandedGroupTemplate 是另一个 marker (与列表里出现的 group 不匹配) →
	// 退化到普通 collapse 行为。
	out2 := collapseGroupMembersIfMixed(in, `$AA("other", "Y")`)
	if len(out2) != 2 {
		t.Fatalf("expected 2 candidates (1 nav + 1 other) when expandedGroupTemplate mismatches, got %d", len(out2))
	}
	if !out2[0].IsGroup {
		t.Fatalf("idx 0 should be collapsed nav, got %+v", out2[0])
	}
	if out2[1].Text != "码表候选" {
		t.Fatalf("idx 1 should be '码表候选', got %+v", out2[1])
	}
}

// TestCollapseGroupMembersIfMixed_EmptyAndNoMember 边界:
//   - 空切片 → 原样返回
//   - 全是普通候选 → 原样返回
func TestCollapseGroupMembersIfMixed_EmptyAndNoMember(t *testing.T) {
	if got := collapseGroupMembersIfMixed(nil, ""); got != nil {
		t.Fatalf("empty input should return nil, got %v", got)
	}
	in := []candidate.Candidate{other("a", 100), other("b", 50)}
	out := collapseGroupMembersIfMixed(in, "")
	if len(out) != 2 || out[0].Text != "a" || out[1].Text != "b" {
		t.Fatalf("no group member: should be no-op, got %+v", out)
	}
}

// TestCollapseGroupMembersIfMixed_GroupCodeEmptyOnMember 防御性: 即使候选标
// IsGroupMember=true 但 GroupCode 为空, 也不会触发 collapse (视为普通候选)。
func TestCollapseGroupMembersIfMixed_GroupCodeEmptyOnMember(t *testing.T) {
	in := []candidate.Candidate{
		{Text: "a", IsGroupMember: true}, // 无 GroupCode
		other("b", 50),
	}
	out := collapseGroupMembersIfMixed(in, "")
	if len(out) != 2 {
		t.Fatalf("expected 2 (no collapse without GroupCode), got %d", len(out))
	}
	for _, c := range out {
		if c.IsGroup {
			t.Fatalf("should not collapse member without GroupCode, got %+v", c)
		}
	}
}

// TestCollapseGroupMembersIfMixed_NavAlreadyPresent 输入里已经存在 IsGroup=true
// 的 nav (来自 PhraseLayer SearchCommand 前缀路径) 时, 仅 collapse member, 不
// 重复生成 nav。
func TestCollapseGroupMembersIfMixed_NavAlreadyPresent(t *testing.T) {
	in := []candidate.Candidate{
		{Text: "圆数字", IsGroup: true, GroupCode: "zzsz", IsPhrase: true},
		member("①", "zzsz", "圆数字", 3000),
		other("码表候选", 100),
	}
	out := collapseGroupMembersIfMixed(in, "")
	// 混合 → member 应被 collapse 成 nav, 但已存在的 nav 也保留 →
	// 结果会有两条 IsGroup=true 候选指向同一 zzsz, 这是上游 (PhraseLayer
	// SearchPrefix 与 SearchCommand 不会同时返回, dict.Composite 也只调一次)
	// 通常不发生; 这里只测试 collapse 不会破坏已有 nav。
	navCount := 0
	for _, c := range out {
		if c.IsGroup && c.GroupCode == "zzsz" {
			navCount++
		}
	}
	if navCount < 1 {
		t.Fatalf("expected at least one nav for zzsz, got %d", navCount)
	}
	// 其它候选必须保留
	hasOther := false
	for _, c := range out {
		if c.Text == "码表候选" {
			hasOther = true
		}
	}
	if !hasOther {
		t.Fatalf("non-group candidate '码表候选' should be preserved")
	}
}

// TestExpandAACandidates_UserDictPrefixKeepsMeta 验证 user dict 存的字面 $AA
// marker 在 expandAACandidates prefix 分支被转 nav 时, **不**强制标 IsPhrase=true,
// 也**不**附 phrase: 命名空间 ID — 保留 Meta.IsUserDict=true, 让 UI 文案走"删除
// 用户词"分支, 删除走源词库 Remove (不是 DisablePhrase)。问题 2 修复。
// 详见 docs/design/candidate-actions.md §2.1。
func TestExpandAACandidates_UserDictPrefixKeepsMeta(t *testing.T) {
	const groupTpl = `$AA("标点符号", "，。")`
	in := []candidate.Candidate{
		{
			Text:   groupTpl,
			Code:   "zzbd",
			Weight: 1000,
			Meta:   candidate.CandidateMeta{IsUserDict: true},
		},
	}
	out := expandAACandidates(in, "zz") // 前缀场景
	if len(out) != 1 {
		t.Fatalf("expected 1 nav, got %d", len(out))
	}
	nav := out[0]
	if !nav.IsGroup {
		t.Fatalf("nav should have IsGroup=true, got %+v", nav)
	}
	if nav.IsPhrase {
		t.Errorf("user dict 来源 nav 不应强标 IsPhrase=true, got %+v", nav)
	}
	if nav.ID != "" {
		t.Errorf("user dict 来源 nav 不应附 phrase: ID, got %q", nav.ID)
	}
	if !nav.Meta.IsUserDict {
		t.Errorf("Meta.IsUserDict 应被保留, got %+v", nav.Meta)
	}
	if nav.PhraseTemplate != groupTpl {
		t.Errorf("PhraseTemplate 应保留原 marker (delete 时用), got %q want %q",
			nav.PhraseTemplate, groupTpl)
	}
}

// TestExpandAACandidates_PhraseLayerPrefixGetsPhraseID 对照: PhraseLayer 来源
// (IsPhrase=true) 的 $AA marker 在 prefix 分支仍附 phrase: ID, 走 DisablePhrase
// 删除路径。原 nav id 命名空间设计不变。
func TestExpandAACandidates_PhraseLayerPrefixGetsPhraseID(t *testing.T) {
	const groupTpl = `$AA("标点符号", "，。")`
	in := []candidate.Candidate{
		{
			Text:     groupTpl,
			Code:     "zzbd",
			Weight:   1000,
			IsPhrase: true, // PhraseLayer 来源
		},
	}
	out := expandAACandidates(in, "zz")
	if len(out) != 1 || !out[0].IsGroup {
		t.Fatalf("expected 1 nav with IsGroup=true, got %+v", out)
	}
	if out[0].ID == "" {
		t.Errorf("PhraseLayer 来源 nav 应附 phrase: ID, got empty")
	}
}

// TestCollapseGroupMembersIfMixed_NavInheritsIDFromGroupTemplate 验证 collapse
// 生成的 nav 候选携带稳定 ID (PhraseCandidateID(groupCode, groupTemplate)),
// 让 Shadow pin 跨 collapse 状态能命中。bug 1 直接根因覆盖。
// 详见 docs/design/candidate-actions.md §5。
func TestCollapseGroupMembersIfMixed_NavInheritsIDFromGroupTemplate(t *testing.T) {
	// 用同一 GroupTemplate 模拟 first member 的字段, collapse 后 nav 应用该 template 推导 ID。
	const groupTpl = `$AA("标点符号", "，。！")`
	m := member("，", "zzbd", "标点符号", 2500)
	m.GroupTemplate = groupTpl
	in := []candidate.Candidate{
		other("用户词", 100),
		m,
		func() candidate.Candidate {
			c := member("。", "zzbd", "标点符号", 2500)
			c.GroupTemplate = groupTpl
			return c
		}(),
	}
	out := collapseGroupMembersIfMixed(in, "")
	var nav *candidate.Candidate
	for i := range out {
		if out[i].IsGroup {
			nav = &out[i]
			break
		}
	}
	if nav == nil {
		t.Fatalf("expected nav collapsed, got none in %+v", out)
	}
	wantID := dict.PhraseCandidateID("zzbd", groupTpl)
	if nav.ID != wantID {
		t.Fatalf("nav ID = %q, want %q (from GroupTemplate)", nav.ID, wantID)
	}
	if nav.PhraseTemplate != groupTpl {
		t.Fatalf("nav PhraseTemplate = %q, want %q", nav.PhraseTemplate, groupTpl)
	}
	if nav.GroupTemplate != groupTpl {
		t.Fatalf("nav GroupTemplate = %q, want %q", nav.GroupTemplate, groupTpl)
	}
}

// TestCollapseThenApplyShadowPins_NavPinTakesEffect bug 2 回归测试:
// collapse 后用 ApplyShadowPins 给 nav 写 pin, nav 应该排到 position 0,
// 跟"用户在 IME 里点 nav 前移到顶"语义一致。验证 collapse 后二次 ApplyShadowPins 链路。
// 详见 docs/design/candidate-actions.md §3.2。
func TestCollapseThenApplyShadowPins_NavPinTakesEffect(t *testing.T) {
	const groupTpl = `$AA("标点符号", "，。！")`
	m1 := member("，", "zzbd", "标点符号", 2500)
	m1.GroupTemplate = groupTpl
	m2 := member("。", "zzbd", "标点符号", 2500)
	m2.GroupTemplate = groupTpl
	in := []candidate.Candidate{
		other("用户词A", 100),
		m1,
		m2,
	}
	out := collapseGroupMembersIfMixed(in, "")
	// 此时顺序应为 [用户词A, nav 标点] (混合 collapse), nav 排在 first member 位置
	if len(out) != 2 {
		t.Fatalf("expected 2 (1 other + 1 nav) after collapse, got %d: %+v", len(out), out)
	}
	if out[0].Text != "用户词A" || !out[1].IsGroup {
		t.Fatalf("collapse pre-pin order wrong: %+v", out)
	}

	// 模拟用户点击 nav "前移到顶": Shadow pin nav.ID → position 0
	navID := dict.PhraseCandidateID("zzbd", groupTpl)
	rules := &dict.ShadowRules{
		Pinned: []dict.PinnedWord{
			{Word: "标点符号", CandID: navID, Position: 0},
		},
	}
	out2 := dict.ApplyShadowPins(out, rules)
	if len(out2) != 2 {
		t.Fatalf("ApplyShadowPins should preserve length, got %d", len(out2))
	}
	// nav 应排到 position 0
	if !out2[0].IsGroup || out2[0].GroupCode != "zzbd" {
		t.Fatalf("nav should be pinned to position 0 after ApplyShadowPins, got out2[0]=%+v", out2[0])
	}
	if out2[1].Text != "用户词A" {
		t.Fatalf("non-nav candidate should move to position 1, got out2[1]=%+v", out2[1])
	}
}
