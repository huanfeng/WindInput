package ui

import (
	"testing"

	"github.com/huanfeng/wind_input/pkg/theme"
)

// horizontalListGap 走真实 theme 路径（ResolveCandidateViews）构建横排树，返回候选列表行的 Gap
// （scale=1，等于 boxGap 逻辑像素）。用小 item padding(2) 使 boxGap=max(spacing-4,0)>0，可区分 12 vs 16。
func horizontalListGap(t *testing.T, indexStyle string) int {
	t.Helper()
	cfg := parityConfig()
	cfg.IndexStyle = indexStyle
	r := NewRenderer(cfg)
	if r.TextDrawer() == nil {
		t.Skip("无可用文本后端")
	}
	views := themePathViews(8, 2) // item padding=2
	r.resolvedV25 = &theme.ResolvedV25{Palette: themePathPalette()}
	r.themeViews = &views
	r.refreshResolvedViews()
	cands := []Candidate{{Text: "中", Index: 1}, {Text: "文", Index: 2}}
	tree := r.buildHorizontalCandidateTree(cands, "", 0, 1, 1, 0, -1, "")
	list := tree.root.Children[len(tree.root.Children)-1] // 无 preedit 时列表行=最后一个 band
	return list.Gap
}

// TestBuildHorizontal_SpacingFollowsTheme 验证间距完全由主题 metrics.item_spacing 决定，
// 与序号样式（circle/text）无关。旧的「文本序号 +4」magic 已移除并下沉到主题文件（msime
// 显式 item_spacing:16），使渲染完全遵循主题文件。
func TestBuildHorizontal_SpacingFollowsTheme(t *testing.T) {
	circleGap := horizontalListGap(t, "circle") // spacing 12, padding 2 → boxGap max(12-4,0)=8
	textGap := horizontalListGap(t, "text")     // 同上：序号样式不再影响间距
	if circleGap != textGap {
		t.Errorf("序号样式不应影响间距（+4 magic 已移除）：circle=%d text=%d", circleGap, textGap)
	}
	if circleGap != 8 {
		t.Errorf("boxGap 应为 8（spacing 12 − padL2 − padR2），got %d", circleGap)
	}
}
