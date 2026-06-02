package ui

// 盒模型引擎的生产形态入口：构建 View 树 → 布局 → 绘制 → 提取命中矩形。
// 盒模型 View 引擎是候选窗唯一渲染路径（旧固定化渲染器已退役）。

import (
	"image"
)

// renderHorizontalV2 用盒模型引擎渲染横排候选窗。
func (r *Renderer) renderHorizontalV2(
	candidates []Candidate,
	input string,
	cursorPos, page, totalPages, hoverIndex int,
	hoverPageBtn string,
	selectedIndex int,
) (*image.RGBA, *RenderResult) {
	tree := r.buildHorizontalCandidateTree(candidates, input, cursorPos, page, totalPages, selectedIndex, hoverIndex, hoverPageBtn)
	return r.renderTree(tree)
}

// renderVerticalV2 用盒模型引擎渲染竖排候选窗。
func (r *Renderer) renderVerticalV2(
	candidates []Candidate,
	input string,
	cursorPos, page, totalPages, hoverIndex int,
	hoverPageBtn string,
	selectedIndex int,
) (*image.RGBA, *RenderResult) {
	tree := r.buildVerticalCandidateTree(candidates, input, cursorPos, page, totalPages, selectedIndex, hoverIndex, hoverPageBtn)
	return r.renderTree(tree)
}

// renderTree 对已构建的候选窗 View 树执行布局 → 绘制 → 命中矩形提取。
// 窗口根铺满画布，仅为投影偏移在右下留出空间（画布即窗口尺寸 + 投影 2px）。
func (r *Renderer) renderTree(tree *candWindowTree) (*image.RGBA, *RenderResult) {
	td := r.textDrawer
	root := tree.root
	Layout(root, 0, 0, td)

	ext := 0
	if root.Shadow != nil {
		ext = maxInt(root.Shadow.OffsetX, root.Shadow.OffsetY)
	}
	w := root.Rect().Dx() + ext
	h := root.Rect().Dy() + ext
	dc, img := r.acquireDrawContext(w, h)
	PaintTree(root, dc, img, td)
	DrawDebugBanner(img)

	res := &RenderResult{Rects: make([]CandidateRect, len(tree.items))}
	for i, it := range tree.items {
		res.Rects[i] = rectOf(i, it)
	}
	if tree.pagerUp != nil {
		rc := rectOf(0, tree.pagerUp)
		res.PageUpRect = &rc
	}
	if tree.pagerDown != nil {
		rc := rectOf(0, tree.pagerDown)
		res.PageDownRect = &rc
	}
	return img, res
}

func rectOf(index int, v *View) CandidateRect {
	r := v.Rect()
	return CandidateRect{
		Index: index,
		X:     float64(r.Min.X),
		Y:     float64(r.Min.Y),
		W:     float64(r.Dx()),
		H:     float64(r.Dy()),
	}
}

func maxInt(a, b int) int {
	if a > b {
		return a
	}
	return b
}
