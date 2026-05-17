package ui

import (
	"testing"
)

// computeMenuDisableState mirrors the logic in handleRightClick for testability.
// Returns (isGlobalFirst, isGlobalLast).
func computeMenuDisableState(pageStartIndex, hitIndex, totalCandidateCount int) (bool, bool) {
	globalIndex := pageStartIndex + hitIndex
	isGlobalFirst := globalIndex == 0
	isGlobalLast := totalCandidateCount <= 0 || globalIndex >= totalCandidateCount-1
	return isGlobalFirst, isGlobalLast
}

func TestMenuDisable_SinglePage(t *testing.T) {
	// 7 candidates on a single page (page 1, candidatesPerPage=7)
	total := 7
	pageStart := 0

	tests := []struct {
		name      string
		hitIndex  int
		wantFirst bool
		wantLast  bool
	}{
		{"first item", 0, true, false},
		{"middle item", 3, false, false},
		{"last item", 6, false, true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			isFirst, isLast := computeMenuDisableState(pageStart, tt.hitIndex, total)
			if isFirst != tt.wantFirst {
				t.Errorf("isGlobalFirst = %v, want %v", isFirst, tt.wantFirst)
			}
			if isLast != tt.wantLast {
				t.Errorf("isGlobalLast = %v, want %v", isLast, tt.wantLast)
			}
		})
	}
}

func TestMenuDisable_MultiPage(t *testing.T) {
	// 20 candidates, 7 per page → 3 pages
	total := 20
	perPage := 7

	tests := []struct {
		name      string
		page      int // 1-based
		hitIndex  int // 0-based within page
		wantFirst bool
		wantLast  bool
	}{
		// Page 1: globalIndex 0-6
		{"page1 first", 1, 0, true, false},
		{"page1 middle", 1, 3, false, false},
		{"page1 last", 1, 6, false, false}, // NOT global last

		// Page 2: globalIndex 7-13
		{"page2 first", 2, 0, false, false}, // NOT global first
		{"page2 middle", 2, 3, false, false},
		{"page2 last", 2, 6, false, false}, // NOT global last

		// Page 3: globalIndex 14-19 (6 candidates on last page)
		{"page3 first", 3, 0, false, false},
		{"page3 last", 3, 5, false, true}, // global last (index 19)
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			pageStart := (tt.page - 1) * perPage
			isFirst, isLast := computeMenuDisableState(pageStart, tt.hitIndex, total)
			if isFirst != tt.wantFirst {
				t.Errorf("isGlobalFirst = %v, want %v (globalIndex=%d)", isFirst, tt.wantFirst, pageStart+tt.hitIndex)
			}
			if isLast != tt.wantLast {
				t.Errorf("isGlobalLast = %v, want %v (globalIndex=%d, total=%d)", isLast, tt.wantLast, pageStart+tt.hitIndex, total)
			}
		})
	}
}

func TestMenuDisable_ZeroTotal(t *testing.T) {
	// Edge case: totalCandidateCount == 0 (uninitialized)
	// All items should have both move-up and move-down disabled
	isFirst, isLast := computeMenuDisableState(0, 0, 0)
	if !isFirst {
		t.Error("expected isGlobalFirst=true when total=0")
	}
	if !isLast {
		t.Error("expected isGlobalLast=true when total=0")
	}
}

func TestMenuDisable_SingleCandidate(t *testing.T) {
	// Only 1 candidate total
	isFirst, isLast := computeMenuDisableState(0, 0, 1)
	if !isFirst {
		t.Error("expected isGlobalFirst=true for single candidate")
	}
	if !isLast {
		t.Error("expected isGlobalLast=true for single candidate")
	}
}

// computeMenuDisableForGroupMember 复制 handleRightClick 中针对 IsGroupMember
// 的 disable 规则, 验证 $AA/$SS 字符组/字符串组子项所有可编辑菜单都被屏蔽。
// 入参分别对应 window_mouse.go 内的 isGlobalFirst / isGlobalLast / 单候选 /
// pinyin / command / quickInput / hasShadow / isSingleChar / isGroupMember。
func computeMenuDisableForGroupMember(
	isGlobalFirst, isGlobalLast, isSingleCandidate,
	isPinyin, isCommand, isQuickInput, hasShadow, isSingleChar, isGroupMember bool,
) (disableMoveUp, disableMoveDown, disableTop, disableDelete, disableReset bool) {
	disableMoveUp = isGlobalFirst || isSingleCandidate || (isPinyin && !isCommand) || isQuickInput || isGroupMember
	disableMoveDown = isGlobalLast || isSingleCandidate || (isPinyin && !isCommand) || isQuickInput || isGroupMember
	disableTop = isGlobalFirst || isQuickInput || isGroupMember
	disableDelete = (isSingleChar && !isCommand) || isQuickInput || isGroupMember
	disableReset = !hasShadow || isGroupMember
	return
}

// TestMenuDisable_GroupMemberAllDisabled 验证 $AA/$SS 字符组/字符串组的子项候选
// 右键菜单 pin/delete/前移/后移/置顶/恢复默认 全 disable, 即使其它条件都允许。
//
// 引入: 2026-05-17 R2 follow-up (字符组顺序在源短语中已完整定义,
// 走"编辑短语"路径而非 Shadow 双轨漂移)。
func TestMenuDisable_GroupMemberAllDisabled(t *testing.T) {
	// 中段、命令候选、有 Shadow、非快捷输入 — 所有 ungated 条件都允许操作,
	// 但 isGroupMember=true 应让所有可编辑项 disable。
	up, down, top, del, reset := computeMenuDisableForGroupMember(
		false, // isGlobalFirst
		false, // isGlobalLast
		false, // isSingleCandidate
		false, // isPinyin
		true,  // isCommand
		false, // isQuickInput
		true,  // hasShadow
		false, // isSingleChar
		true,  // isGroupMember
	)
	if !up || !down || !top || !del || !reset {
		t.Errorf("$AA/$SS group member: all menu actions must be disabled, got up=%v down=%v top=%v del=%v reset=%v",
			up, down, top, del, reset)
	}
}

// TestMenuDisable_NonGroupMemberHonorsOtherRules 回归测试: IsGroupMember=false
// 的命令候选仍按现有规则启用 (不被新逻辑无意中卡住)。
func TestMenuDisable_NonGroupMemberHonorsOtherRules(t *testing.T) {
	// 中段、命令候选、有 Shadow — 期望全部启用
	up, down, top, del, reset := computeMenuDisableForGroupMember(
		false, false, false, false, true, false, true, false, false,
	)
	if up || down || top || del || reset {
		t.Errorf("non-group-member: middle command with shadow should enable all, got up=%v down=%v top=%v del=%v reset=%v",
			up, down, top, del, reset)
	}
}
