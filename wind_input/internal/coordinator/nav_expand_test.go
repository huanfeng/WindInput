package coordinator

import (
	"testing"

	"github.com/huanfeng/wind_input/internal/ui"
)

// TestSelectNavCandidate_SetsexpandedGroupTemplate 验证 doSelectCandidate 在
// IsGroup 分支:
//   - 当 cand.GroupCode == inputBuffer (collapsed nav 场景): 仅置 expandedGroupTemplate,
//     不再改 buffer。
//   - 当 cand.GroupCode != inputBuffer (前缀 nav 场景): 替换 buffer 并置位
//     expandedGroupTemplate, 以避免下一次 update 又 collapse 把字符组收起来。
//
// 不构造真实候选 / 真实 engine; doSelectCandidate 调用的 updateCandidates 在
// engineMgr 装上 stub 后, 仅做 stub 查询不会 panic。showUI / hideUI 在 uiManager
// 为 nil 时直接 return。
func TestSelectNavCandidate_SetsexpandedGroupTemplate(t *testing.T) {
	const groupTpl = `$AA("标点符号", "，。")`
	t.Run("collapsed_nav_same_buffer", func(t *testing.T) {
		h := newTestCoordinator(t, withEngineMgr(withCodetableEntry("zzbd", "标点")))
		// 用户输入 zzbd, candidates 里第一个是 collapsed nav (与 buffer 同 code)
		h.inputBuffer = "zzbd"
		h.inputCursorPos = len("zzbd")
		h.candidates = []ui.Candidate{{
			Text: "标点符号", Code: "zzbd", IsGroup: true, GroupCode: "zzbd", GroupName: "标点符号",
			GroupTemplate: groupTpl,
		}}
		h.expandedGroupTemplate = ""

		_ = h.doSelectCandidate(0)

		if h.expandedGroupTemplate != groupTpl {
			t.Fatalf("expandedGroupTemplate should be set to %q, got %q", groupTpl, h.expandedGroupTemplate)
		}
		if h.inputBuffer != "zzbd" {
			t.Fatalf("inputBuffer must remain zzbd (no replace when nav.code == buffer), got %q", h.inputBuffer)
		}
	})

	t.Run("prefix_nav_replaces_buffer", func(t *testing.T) {
		h := newTestCoordinator(t, withEngineMgr(withCodetableEntry("zzbd", "标点")))
		// 用户输入 zz, 前缀 nav 候选 GroupCode=zzbd
		h.inputBuffer = "zz"
		h.inputCursorPos = len("zz")
		h.candidates = []ui.Candidate{{
			Text: "标点符号", Code: "zzbd", IsGroup: true, GroupCode: "zzbd", GroupName: "标点符号",
			GroupTemplate: groupTpl,
		}}
		h.expandedGroupTemplate = ""

		_ = h.doSelectCandidate(0)

		if h.inputBuffer != "zzbd" {
			t.Fatalf("inputBuffer should be replaced with zzbd, got %q", h.inputBuffer)
		}
		if h.expandedGroupTemplate != groupTpl {
			t.Fatalf("expandedGroupTemplate should be set to %q to prevent re-collapse, got %q", groupTpl, h.expandedGroupTemplate)
		}
	})
}

// TestClearStateClearsexpandedGroupTemplate 验证 clearState() 清零 expandedGroupTemplate,
// 这是状态机第三条 invariant: 输入流重置后下一轮回到默认 collapse 行为。
func TestClearStateClearsexpandedGroupTemplate(t *testing.T) {
	h := newTestCoordinator(t, withEngineMgr())
	h.expandedGroupTemplate = `$AA("foo", "bar")`
	h.inputBuffer = "zzbd"
	h.clearState()
	if h.expandedGroupTemplate != "" {
		t.Fatalf("clearState should clear expandedGroupTemplate, got %q", h.expandedGroupTemplate)
	}
}

// TestAlphaKeyClearsexpandedGroupTemplate 验证 buffer 通过 handleAlphaKey 变化时
// 清零 expandedGroupTemplate (第三条 invariant 的另一种触发路径)。
func TestAlphaKeyClearsexpandedGroupTemplate(t *testing.T) {
	h := newTestCoordinator(t, withEngineMgr(withCodetableEntry("zzbd", "标点")))
	h.expandedGroupTemplate = `$AA("foo", "bar")`
	h.inputBuffer = "zzbd"
	h.inputCursorPos = len("zzbd")

	// 模拟敲下一个字母 "a", handleAlphaKey 会把 buffer 改成 "zzbda"
	_ = h.handleAlphaKey("a")

	if h.expandedGroupTemplate != "" {
		t.Fatalf("handleAlphaKey should clear expandedGroupTemplate when buffer mutates, got %q", h.expandedGroupTemplate)
	}
}
