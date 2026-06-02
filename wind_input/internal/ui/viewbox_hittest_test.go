package ui

import (
	"image"
	"testing"

	"github.com/huanfeng/wind_input/pkg/config"
)

// hitTest 复刻 window_mouse.go 的命中判定：先匹配候选矩形（首个命中即返回），
// 未命中候选时再测翻页按钮（先上一页，后下一页）。返回 (候选索引, 翻页按钮)。
// 候选未命中时索引为 -1；翻页未命中时按钮为 ""。
func hitTest(res *RenderResult, mx, my float64) (int, string) {
	for _, rect := range res.Rects {
		if mx >= rect.X && mx <= rect.X+rect.W &&
			my >= rect.Y && my <= rect.Y+rect.H {
			return rect.Index, ""
		}
	}
	if r := res.PageUpRect; r != nil && mx >= r.X && mx <= r.X+r.W && my >= r.Y && my <= r.Y+r.H {
		return -1, "up"
	}
	if r := res.PageDownRect; r != nil && mx >= r.X && mx <= r.X+r.W && my >= r.Y && my <= r.Y+r.H {
		return -1, "down"
	}
	return -1, ""
}

func rectCenter(r CandidateRect) (float64, float64) {
	return r.X + r.W/2, r.Y + r.H/2
}

func inBounds(r CandidateRect, img *image.RGBA) bool {
	b := img.Bounds()
	return r.X >= 0 && r.Y >= 0 &&
		r.X+r.W <= float64(b.Dx()) && r.Y+r.H <= float64(b.Dy())
}

// assertHitRects 对渲染结果做通用命中精度断言：
//   - 候选数量/索引一一对应
//   - 每个候选中心点命中自身索引
//   - 所有命中矩形在画布内
//   - 多页时翻页矩形存在、在画布内、中心命中对应按钮且不误入候选
func assertHitRects(t *testing.T, label string, img *image.RGBA, res *RenderResult, wantN int, multiPage bool) {
	t.Helper()
	if len(res.Rects) != wantN {
		t.Fatalf("%s: len(Rects)=%d, want %d", label, len(res.Rects), wantN)
	}
	for i, rect := range res.Rects {
		if rect.Index != i {
			t.Errorf("%s: Rects[%d].Index=%d, want %d", label, i, rect.Index, i)
		}
		if !inBounds(rect, img) {
			t.Errorf("%s: 候选矩形[%d] %+v 越出画布 %v", label, i, rect, img.Bounds())
		}
		cx, cy := rectCenter(rect)
		if idx, btn := hitTest(res, cx, cy); idx != i || btn != "" {
			t.Errorf("%s: 候选[%d] 中心(%.1f,%.1f) 命中 idx=%d btn=%q, want idx=%d", label, i, cx, cy, idx, btn, i)
		}
	}

	if !multiPage {
		return
	}
	if res.PageUpRect == nil || res.PageDownRect == nil {
		t.Fatalf("%s: 多页应有翻页矩形, got up=%v down=%v", label, res.PageUpRect, res.PageDownRect)
	}
	for name, pr := range map[string]*CandidateRect{"up": res.PageUpRect, "down": res.PageDownRect} {
		if !inBounds(*pr, img) {
			t.Errorf("%s: 翻页矩形 %s %+v 越出画布 %v", label, name, *pr, img.Bounds())
		}
		cx, cy := rectCenter(*pr)
		if idx, btn := hitTest(res, cx, cy); btn != name || idx != -1 {
			t.Errorf("%s: 翻页 %s 中心(%.1f,%.1f) 命中 idx=%d btn=%q, want btn=%q", label, name, cx, cy, idx, btn, name)
		}
	}
}

// TestViewEngine_HitTest_Horizontal 横排命中矩形回归：候选中心命中自身、翻页区命中正确。
func TestViewEngine_HitTest_Horizontal(t *testing.T) {
	r := NewRenderer(parityConfig())
	if r.TextDrawer() == nil {
		t.Skip("无可用文本后端")
	}
	applyThemePath(r, 6, 8) // 真实主题路径：填充 resolvedViews 字号/几何（生产环境总有主题）
	cands := []Candidate{
		{Text: "中文", Index: 1},
		{Text: "中", Index: 2, Comment: "zhōng"},
		{Text: "众", Index: 3},
		{Text: "种", Index: 4},
		{Text: "重", Index: 5},
	}
	// page=2/totalPages=3：上一页、下一页均可用，pager 两按钮都在
	img, res := r.renderHorizontalV2(cands, "zhong", 5, 2, 3, -1, "", 0)
	assertHitRects(t, "horizontal", img, res, len(cands), true)
}

// TestViewEngine_HitTest_Vertical 竖排命中矩形回归：每行候选中心命中自身、翻页区命中正确。
func TestViewEngine_HitTest_Vertical(t *testing.T) {
	cfg := parityConfig()
	cfg.Layout = config.LayoutVertical
	r := NewRenderer(cfg)
	if r.TextDrawer() == nil {
		t.Skip("无可用文本后端")
	}
	applyThemePath(r, 6, 8) // 真实主题路径：填充 resolvedViews 字号/几何
	cands := []Candidate{
		{Text: "中文", Index: 1},
		{Text: "中", Index: 2, Comment: "zhōng"},
		{Text: "众", Index: 3},
		{Text: "种", Index: 4},
		{Text: "重", Index: 5},
	}
	img, res := r.renderVerticalV2(cands, "zhong", 5, 2, 3, -1, "", 0)
	assertHitRects(t, "vertical", img, res, len(cands), true)
}

// TestViewEngine_HitTest_SinglePage 单页无翻页：不应产生翻页矩形，候选命中正常。
func TestViewEngine_HitTest_SinglePage(t *testing.T) {
	r := NewRenderer(parityConfig())
	if r.TextDrawer() == nil {
		t.Skip("无可用文本后端")
	}
	applyThemePath(r, 6, 8) // 真实主题路径：填充 resolvedViews 字号/几何
	cands := []Candidate{{Text: "中文", Index: 1}, {Text: "中", Index: 2}, {Text: "众", Index: 3}}
	img, res := r.renderHorizontalV2(cands, "zhong", 5, 1, 1, -1, "", 0)
	assertHitRects(t, "single-page", img, res, len(cands), false)
	if res.PageUpRect != nil || res.PageDownRect != nil {
		t.Errorf("single-page: 单页不应有翻页矩形, got up=%v down=%v", res.PageUpRect, res.PageDownRect)
	}
}
