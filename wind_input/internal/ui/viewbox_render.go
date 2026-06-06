package ui

// 盒模型引擎的生产形态入口：构建 View 树 → 布局 → 绘制 → 提取命中矩形。
// 盒模型 View 引擎是候选窗唯一渲染路径（旧固定化渲染器已退役）。

import (
	"image"
	"math"

	"github.com/gogpu/gg"
	"github.com/huanfeng/wind_input/pkg/theme"
)

// refreshResolvedViews 重建 r.resolvedViews，由渲染入口 render*V2 每次调用。
// 候选窗外观直接消费 theme 包解析结果 ResolveCandidateViews（views 已 merge defaultViews
// 基线、几何+颜色权威）；字号/行高/竖排宽是运行时值（用户全局字号派生 + DPI scale），在此回填。
// 无主题（仅测试路径会出现：未调 SetTheme）时不改 r.resolvedViews，由测试自行预填。
func (r *Renderer) refreshResolvedViews() {
	if r.resolvedV3 == nil || r.themeViews == nil {
		return
	}
	r.resolvedViews = theme.ResolveCandidateViews(*r.themeViews, r.resolvedV3.Palette)
	// P7-B：主题 views 显式字号（逻辑像素，ResolveCandidateViews 填入）优先并 ×DPI scale；
	// 未写（0）则回退运行时派生（用户全局字号 + DPI，已含 scale）。
	scale := GetDPIScale()
	// 相对字号：各元素字号 = 主候选字体(base) + 主题配置的有符号偏移(逻辑px)，再 ×DPI。
	// ResolveCandidateViews 已把 views.<el>.font_size 解析为偏移量填入 rv.<el>.FontSize；
	// 偏移 0（含 text/preedit 默认）即等于主字体。零魔法数字——差值全在主题/基线配置里。
	base := r.config.FontSize // 主候选字体(device px) = 用户基准字号 × scale
	r.resolvedViews.Text.FontSize = base + r.resolvedViews.Text.FontSize*scale
	r.resolvedViews.PreeditBar.FontSize = base + r.resolvedViews.PreeditBar.FontSize*scale
	r.resolvedViews.Index.FontSize = base + r.resolvedViews.Index.FontSize*scale
	r.resolvedViews.Comment.FontSize = base + r.resolvedViews.Comment.FontSize*scale
	r.resolvedViews.FooterBar.FontSize = base + r.resolvedViews.FooterBar.FontSize*scale
	r.resolvedViews.ModeLabel.FontSize = base + r.resolvedViews.ModeLabel.FontSize*scale
	r.resolvedViews.ItemHeight = r.config.ItemHeight
	// 竖排最大宽（哲学Y 双层覆盖）：
	//   1. r.config.VerticalMaxWidth>0：运行时强制覆盖（目前仅测试设置），最高优先；
	//   2. 否则 verticalMaxWidthFollowTheme ? 主题 behavior.vertical_max_width : config.UI 用户值。
	themeVMW := float64(r.resolvedV3.Behavior.VerticalMaxWidth)
	effVMW := themeVMW
	if !r.verticalMaxWidthFollowTheme && r.userVerticalMaxWidth > 0 {
		effVMW = r.userVerticalMaxWidth
	}
	r.resolvedViews.VerticalMaxWidth = pickF(r.config.VerticalMaxWidth, effVMW)
}

// newSharedDrawContext 创建 dc 与 img 实时共享同一像素缓冲的绘制上下文（独立窗口用，
// 每次新建 buffer，不复用 scratch；候选窗高频路径用 (*Renderer).acquireDrawContext）。
// 注意：gogpu/gg 的 NewContext().Image() 返回快照而非实时视图，PaintTree 要求 dc 绘制
// 实时反映到 img，故必须经 pixmap 的 ImageView 共享缓冲。
func newSharedDrawContext(w, h int) (*gg.Context, *image.RGBA) {
	// 纵深防御：尺寸为 0/负（异常主题导致几何塌缩）时 clamp 到 1×1，
	// 避免 gg.NewPixmapFromBuffer panic「width and height must be > 0」（候选窗消失）。
	if w < 1 {
		w = 1
	}
	if h < 1 {
		h = 1
	}
	pm := gg.NewPixmapFromBuffer(make([]byte, w*h*4), w, h)
	return gg.NewContextForPixmap(pm), pm.ImageView()
}

// renderHorizontalV2 用盒模型引擎渲染横排候选窗。
func (r *Renderer) renderHorizontalV2(
	candidates []Candidate,
	input string,
	cursorPos, page, totalPages, hoverIndex int,
	hoverPageBtn string,
	selectedIndex int,
) (*image.RGBA, *RenderResult) {
	r.refreshResolvedViews()
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
	r.refreshResolvedViews()
	tree := r.buildVerticalCandidateTree(candidates, input, cursorPos, page, totalPages, selectedIndex, hoverIndex, hoverPageBtn)
	return r.renderTree(tree)
}

// renderTree 对已构建的候选窗 View 树执行布局 → 绘制 → 命中矩形提取。
// 有 blur/spread 时四向扩展画布以容纳模糊扩散；否则仅右下留出偏移空间（向后兼容）。
func (r *Renderer) renderTree(tree *candWindowTree) (*image.RGBA, *RenderResult) {
	td := r.textDrawer
	root := tree.root

	// 先计算阴影四向 margin，再以 (marginLeft, marginTop) 为起点 layout。
	// blur/spread > 0：四向扩展，offset 决定不对称量；否则退化为旧右下扩展。
	marginLeft, marginTop, marginRight, marginBottom := 0, 0, 0, 0
	if sh := root.Shadow; sh != nil {
		if sh.Blur > 0 || sh.Spread > 0 {
			// 与 paintBlurredShadow 相同的公式：3-sigma 覆盖 99.7% 模糊梯度。
			sigma := math.Sqrt(float64(sh.Blur) * float64(sh.Blur+2))
			pad := int(math.Ceil(3*sigma)) + 2
			base := pad + sh.Spread
			marginLeft = base + maxInt(-sh.OffsetX, 0)
			marginTop = base + maxInt(-sh.OffsetY, 0)
			marginRight = base + maxInt(sh.OffsetX, 0)
			marginBottom = base + maxInt(sh.OffsetY, 0)
		} else {
			marginRight = maxInt(sh.OffsetX, 0)
			marginBottom = maxInt(sh.OffsetY, 0)
		}
	}
	Layout(root, marginLeft, marginTop, td)

	w := root.Rect().Dx() + marginLeft + marginRight
	h := root.Rect().Dy() + marginTop + marginBottom
	// 纵深防御：root 几何塌缩（异常主题）时 clamp，避免 0 尺寸画布 panic 致候选窗消失。
	if w < 1 {
		w = 1
	}
	if h < 1 {
		h = 1
	}
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
