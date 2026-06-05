//go:build windows

package ui

import (
	"image"
	"image/color"
	"log/slog"
	"sync"

	"github.com/huanfeng/wind_input/pkg/theme"
)

// ToastLevel 决定 toast 配色，呼应消息严重程度。
// ToastLevel / ToastPosition / ToastOptions 已迁至 types_neutral.go (平台无关)。

// ToastRenderer 负责把 ToastOptions 渲染成 RGBA 图像。复用 TextBackendManager 的 DirectWrite 后端，
// 与 tooltip / status 渲染保持一致的反锯齿表现。
type ToastRenderer struct {
	TextBackendManager

	mu         sync.Mutex
	resolvedV3 *theme.ResolvedV3
	logger     *slog.Logger
	imgRes     imageResolver // P8 切片6：背景图/layers 解码缓存（与候选窗共享基础设施）
}

// NewToastRenderer 创建 toast 渲染器。默认 DirectWrite, 与项目主配置默认 FontEngine 一致;
// 后续 Manager.SetTextRenderMode 会按用户实际配置统一切换, 避免 toast 与其它组件持有
// 不同后端导致字体在内存中重复加载。
func NewToastRenderer(logger *slog.Logger) *ToastRenderer {
	r := &ToastRenderer{
		TextBackendManager: NewTextBackendManager("toast"),
		logger:             logger,
	}
	r.SetTextRenderMode(TextRenderModeDirectWrite)
	return r
}

// SetTheme 注入解析后的主题，用于颜色取值。
func (r *ToastRenderer) SetTheme(rv *theme.ResolvedV3) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.resolvedV3 = rv
	r.imgRes.reset() // 换主题清空位图缓存（ref 解码结果按主题失效）
}

// Close 释放渲染资源。
func (r *ToastRenderer) Close() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.TextBackendManager.Close()
}

// levelAccent 返回各 Level 对应的边框/标题强调色。背景沿用 tooltip 主题色。
// 配色已提到平台无关的 ToastAccentColor (types_neutral.go), 与 darwin forwarder 共用单一来源。
func levelAccent(level ToastLevel) color.Color {
	return ToastAccentColor(level)
}

// resolveToastNode 计算 Toast 盒模型 RVNode（P8 切片5：几何+border+font+颜色）。
// 颜色：views.toast token > Palette.Toast > 默认深灰底白字；bg 经 forceAlphaOpaque 强制不透明
// （Toast 与系统通知一致，避免重要信息透出底层窗口）。几何/字号由 Render 按现状兜底。
func (r *ToastRenderer) resolveToastNode() theme.RVNode {
	node := theme.RVNode{
		BgColor:   color.RGBA{R: 0x2B, G: 0x2B, B: 0x2B, A: 0xFF},
		TextColor: color.RGBA{R: 0xFF, G: 0xFF, B: 0xFF, A: 0xFF},
	}
	r.mu.Lock()
	rv := r.resolvedV3
	r.mu.Unlock()
	if rv != nil {
		var tn *theme.ViewNode
		if rv.Views != nil {
			tn = rv.Views.Toast
		}
		node = theme.ResolveToastViews(tn, rv.Palette)
	}
	node.BgColor = forceAlphaOpaque(node.BgColor)
	return node
}

// scaledOr 返回 Dimension 缩放后的像素值；零值（未配）回退 def（逻辑像素）×scale。
func scaledOr(d theme.Dimension, def, scale float64) int {
	if v := d.Scaled(scale); v != 0 {
		return v
	}
	return int(def * scale)
}

// forceAlphaOpaque 把任意颜色的 alpha 强制设为 0xFF，避免主题里 tooltip 背景带的轻微半透明
// 在 toast 这种独立通知场景里造成"重要信息透出底层窗口内容"的观感。
func forceAlphaOpaque(c color.Color) color.Color {
	r, g, b, _ := c.RGBA()
	return color.RGBA{R: uint8(r >> 8), G: uint8(g >> 8), B: uint8(b >> 8), A: 0xFF}
}

// Render 把 opts 渲染为 RGBA 图像。返回值已经包含外边距 + 阴影位（如有），调用方按窗口尺寸定位即可。
// maxContentPx 为内容区最大宽度（含 padding），<=0 表示由渲染器自行决定。
func (r *ToastRenderer) Render(opts ToastOptions, maxContentPx int) *image.RGBA {
	if opts.Title == "" && opts.Message == "" {
		return nil
	}

	scale := GetDPIScale()
	node := r.resolveToastNode()

	titleSize := (15.0 + node.FontSize) * scale
	bodySize := (13.0 + node.FontSize) * scale
	// 行距/标题距来自 views.toast（line_spacing/title_gap）；未配兜底现状 4/6（零回归）。
	lineSpacing := float64(scaledOr(node.LineSpacing, 4.0, scale))
	titleGap := float64(scaledOr(node.TitleGap, 6.0, scale)) // 标题与正文之间额外间距
	// 左侧 accent 条参数：完全位于不透明背景内部, 不与圆角外缘相切, 避免反锯齿像素溢出到
	// layered 窗口的透明区域产生"边缘透色"问题。
	accentBarWidth := 4.0 * scale
	accentBarInset := 5.0 * scale // accent 条距 bg 左侧 / 上下圆角的安全距离
	// 文本左侧留白需绕开 accent 条 + 一小段呼吸空间。
	textLeft := accentBarInset + accentBarWidth + 8.0*scale

	// padding/radius：views.toast 未配则兜底现状（12 / 6）。padding.Left 固定 textLeft（为 accent 条留空间）。
	padTop := scaledOr(node.PadTop, 12.0, scale)
	padRight := scaledOr(node.PadRight, 12.0, scale)
	padBottom := scaledOr(node.PadBottom, 12.0, scale)
	radius := scaledOr(node.BorderRadius, 6.0, scale)

	r.mu.Lock()
	td := r.TextDrawer()
	var resources map[string]string
	if r.resolvedV3 != nil {
		resources = r.resolvedV3.Resources
	}
	r.mu.Unlock()
	accent := levelAccent(opts.Level)

	// 计算可用内容宽度（左 textLeft + 右 padRight 之间的可绘区域）。
	var innerMax float64
	if maxContentPx > 0 {
		innerMax = float64(maxContentPx) - textLeft - float64(padRight)
		if innerMax < 80*scale {
			innerMax = 80 * scale
		}
	}

	// 处理正文：按 \n 切行，逐行测量；过宽则截断尾部为 "…"。
	bodyLines := splitLines(opts.Message)
	if innerMax > 0 {
		for i, line := range bodyLines {
			if td.MeasureString(line, bodySize) > innerMax {
				bodyLines[i] = truncateLineToWidth(td, line, bodySize, innerMax)
			}
		}
	}

	// 标题同样需要可能的截断。
	title := opts.Title
	if title != "" && innerMax > 0 {
		if td.MeasureString(title, titleSize) > innerMax {
			title = truncateLineToWidth(td, title, titleSize, innerMax)
		}
	}

	// 计算所有行的最大宽度。
	var contentWidth float64
	if title != "" {
		contentWidth = td.MeasureString(title, titleSize)
	}
	for _, line := range bodyLines {
		if w := td.MeasureString(line, bodySize); w > contentWidth {
			contentWidth = w
		}
	}
	if contentWidth <= 0 {
		return nil
	}

	width := contentWidth + textLeft + float64(padRight)
	if width < 160*scale {
		width = 160 * scale // 太窄不好看
	}

	// 构建 View 树：root 列布局（圆角 bg + padding；padding.Left=textLeft 绕开 accent 条）。
	border := Border{Radius: radius}
	if node.BorderColor != nil {
		border.Color = node.BorderColor
		border.Width = node.BorderWidth.Scaled(scale)
		if border.Width == 0 {
			border.Width = int(1.0 * scale)
		}
	}
	root := &View{
		Layout:     LayoutColumn,
		Gap:        int(lineSpacing),
		Padding:    Edges{Top: padTop, Right: padRight, Bottom: padBottom, Left: int(textLeft)},
		Background: r.imgRes.fillFor(node.BgColor, node.BgImage, node.BgGradient, resources), // P8 切片6：背景可带图/渐变
		Border:     border,
		FixedW:     int(width),
	}
	r.imgRes.appendLayers(root, node.Layers, resources, func(v float64) int { return int(v * scale) }) // P8 切片6：toast 装饰层
	if title != "" {
		// 标题用 accent 颜色（level 运行时色），醒目。
		tv := &View{Text: title, TextStyle: TextStyle{FontSize: titleSize, Color: accent, Weight: node.FontWeight, Family: node.FontFamily}}
		if len(bodyLines) > 0 {
			// 标题与正文额外间距：margin.Bottom + root.Gap(lineSpacing) = titleGap。
			tv.Margin = Edges{Bottom: int(titleGap - lineSpacing)}
		}
		root.Children = append(root.Children, tv)
	}
	for _, line := range bodyLines {
		root.Children = append(root.Children, &View{Text: line, TextStyle: TextStyle{FontSize: bodySize, Color: node.TextColor, Weight: node.FontWeight, Family: node.FontFamily}})
	}

	Layout(root, 0, 0, td)
	w := root.Rect().Dx()
	h := root.Rect().Dy()
	dc, img := newSharedDrawContext(w, h)
	PaintTree(root, dc, img, td)

	// 左侧 accent 条（Fill，完全位于 bg 内部，不接触圆角外缘）。后处理：定位用 root 高度。
	barH := float64(h) - accentBarInset*2
	if barH > 0 {
		dc.SetColor(accent)
		dc.DrawRoundedRectangle(accentBarInset, accentBarInset, accentBarWidth, barH, accentBarWidth/2)
		dc.Fill()
	}

	DrawDebugBanner(img)
	return img
}
