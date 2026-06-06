package ui

// 盒模型 View 渲染引擎 —— 绘制层（measure/arrange 在 viewbox.go）。
//
// 分三趟遍历，原因见 docs/design/archive/theme-view-architecture.md 与旧渲染器的 PHASE1/PHASE2 约定：
//   趟 A paintShapes：投影 → 底色 → 背景图 → layers(z<0) → 递归子节点 → 描边（gg 矢量 + 直接像素）
//   趟 B paintText：在 td.BeginDraw/EndDraw 之间统一绘制所有文本（GDI/DirectWrite 需批处理）
//   趟 C paintOverlays：layers(z>0) 覆盖到文本之上（纯图片，经 theme.DrawBackground 直接写像素）
// 内容基准 z=0：z<0 的覆盖图在趟 A、z>0 在趟 C，故天然实现"内容上/下"分层。

import (
	"image"

	"github.com/gogpu/gg"
	"github.com/huanfeng/wind_input/pkg/theme"
)

// PaintTree 把已 Layout 的 View 树绘制到 (dc, img)。dc 与 img 必须共享同一像素缓冲
// （gg.NewContext 后 dc.Image().(*image.RGBA)，或 NewContextForPixmap 的 ImageView）。
func PaintTree(root *View, dc *gg.Context, img *image.RGBA, td TextDrawer) {
	root.paintShapes(dc, img)
	td.BeginDraw(img)
	root.paintText(td)
	td.EndDraw()
	root.paintOverlays(dc, img)
}

func (v *View) paintShapes(dc *gg.Context, img *image.RGBA) {
	r := v.rect
	x, y := float64(r.Min.X), float64(r.Min.Y)
	w, h := float64(r.Dx()), float64(r.Dy())
	rad := float64(v.Border.Radius)

	// 投影（在底色之前）
	if v.Shadow != nil && v.Shadow.Color != nil {
		dc.SetColor(v.Shadow.Color)
		dc.DrawRoundedRectangle(x+float64(v.Shadow.OffsetX), y+float64(v.Shadow.OffsetY), w, h, rad)
		dc.Fill()
	}

	// 底色（radius=0 即普通矩形）
	if v.Background.Color != nil {
		dc.SetColor(v.Background.Color)
		dc.DrawRoundedRectangle(x, y, w, h, rad)
		dc.Fill()
	}

	// 渐变（底色之上、背景图之下）：按 View rect 现场栅格化为预乘位图，
	// 复用 DrawBackground 的圆角裁剪 + 预乘合成（与背景图同路径）。
	if g := v.Background.Gradient; g != nil {
		if gimg := theme.RasterizeGradient(g, r.Dx(), r.Dy()); gimg != nil {
			theme.DrawBackground(img, r, gimg, "stretch", Edges{}, 1.0, v.Border.Radius)
		}
	}

	// 背景图：
	//   - 全覆盖（默认）：铺满边框盒，传 Border.Radius 让 DrawBackground 按圆角矩形覆盖度裁角——
	//     内部元素（如选中候选项）四周已被窗口底色填满，无法靠 alpha-gate 裁角，必须靠 radius 遮罩。
	//   - 定位（配了 anchor/offset/size）：按 anchor+offset（含百分比）在边框盒内摆放、可缩到 size，
	//     复用 drawLayer 的定位+矩形硬裁逻辑（裁到边框盒、不外溢），与覆盖图层同源。
	if v.Background.Image != nil {
		if v.Background.Positioned {
			bl := ImageLayer{
				Img: v.Background.Image, Mode: v.Background.Mode, Slice: v.Background.Slice,
				Opacity: v.Background.Opacity, Anchor: v.Background.Anchor,
				OffsetX: v.Background.OffsetX, OffsetY: v.Background.OffsetY,
				OffsetXPct: v.Background.OffsetXPct, OffsetYPct: v.Background.OffsetYPct,
				W: v.Background.ImgW, H: v.Background.ImgH,
			}
			drawLayer(dc, img, r, &bl)
		} else {
			theme.DrawBackground(img, r, v.Background.Image, modeOrStretch(v.Background.Mode), v.Background.Slice, opacityOr1(v.Background.Opacity), v.Border.Radius)
		}
	}

	// 覆盖图层 z<0
	for i := range v.Layers {
		if v.Layers[i].Z < 0 {
			drawLayer(dc, img, r, &v.Layers[i])
		}
	}

	// 子节点
	for _, c := range v.Children {
		c.paintShapes(dc, img)
	}

	// 边框：用「外圆角矩形 − 内圆角矩形」的 even-odd 填充环，代替 gg 的中心描边。
	// gg@v0.48.3 的 Stroke 存在 AA 渗色（见库内 TestStroke_*Bleed / FillThenStrokeBleed），
	// 中心描边会致边框粗细不均；填充环只用 Fill——粗细数学上恒为 Width，仅内/外边缘各一条干净 AA。
	// 占位与旧中心描边一致：外圈与边框盒边缘对齐，向内占据 Width 宽（[边缘, 边缘+Width]）。
	if v.Border.Color != nil && v.Border.Width > 0 {
		bw := float64(v.Border.Width)
		innerRad := rad - bw
		if innerRad < 0 {
			innerRad = 0
		}
		dc.SetColor(v.Border.Color)
		dc.DrawRoundedRectangle(x, y, w, h, rad)                      // 外圈
		dc.DrawRoundedRectangle(x+bw, y+bw, w-2*bw, h-2*bw, innerRad) // 内圈（even-odd 挖空）
		dc.SetFillRule(gg.FillRuleEvenOdd)
		dc.Fill()
		dc.SetFillRule(gg.FillRuleNonZero) // 还原默认（dc 全树共享，Fill 默认 nonzero）
	}
}

func (v *View) paintText(td TextDrawer) {
	if v.Text != "" {
		if v.TextStyle.Color != nil {
			r := v.rect
			fs := v.TextStyle.FontSize
			tx := float64(r.Min.X + v.Padding.Left)
			if v.TextStyle.Align == AlignCenter {
				tw := td.MeasureStringFont(v.Text, fs, v.TextStyle.Family)
				tx = float64(r.Min.X) + float64(r.Dx())/2 - tw/2
			}
			// 垂直基线：内容盒竖直中心 + fs/3（与旧渲染器 candY+fontSize/3 一致）
			baselineY := float64(r.Min.Y) + float64(r.Dy())/2 + fs/3
			// P7-B：统一走 DrawStringFull——family 空时内部回退按字重/全局字体绘制（零回归）。
			td.DrawStringFull(v.Text, tx, baselineY, fs, v.TextStyle.Color, v.TextStyle.Weight, v.TextStyle.Family)
		}
		return
	}
	for _, c := range v.Children {
		c.paintText(td)
	}
}

func (v *View) paintOverlays(dc *gg.Context, img *image.RGBA) {
	for i := range v.Layers {
		if v.Layers[i].Z > 0 {
			drawLayer(dc, img, v.rect, &v.Layers[i])
		}
	}
	for _, c := range v.Children {
		c.paintOverlays(dc, img)
	}
}

// drawLayer 把一个覆盖层（图片或纯色）按 anchor+offset+size 定位到 host 矩形内并绘制。
func drawLayer(dc *gg.Context, img *image.RGBA, host image.Rectangle, l *ImageLayer) {
	if l.Img == nil && l.Color == nil {
		return
	}
	w, h := l.W, l.H
	if l.Img != nil {
		if w <= 0 {
			w = l.Img.Bounds().Dx()
		}
		if h <= 0 {
			h = l.Img.Bounds().Dy()
		}
	}
	if w <= 0 || h <= 0 {
		return
	}
	x, y := anchorOffset(host, w, h, l.Anchor)
	// dp 偏移（已 ×scale）+ 百分比偏移（相对 host 对应边长，此刻 host 已知）。
	x += l.OffsetX + int(float64(host.Dx())*l.OffsetXPct/100+0.5)
	y += l.OffsetY + int(float64(host.Dy())*l.OffsetYPct/100+0.5)
	if l.Img != nil {
		// 覆盖层（水印等装饰图）不按 host 圆角裁剪（radius=0）；其自身形状由素材 alpha 决定。
		// 但矩形硬裁到 host：超出边框盒的部分不画（定位/百分比偏移后不外溢）。
		theme.DrawBackgroundClipped(img, image.Rect(x, y, x+w, y+h), l.Img, modeOrStretch(l.Mode), l.Slice, opacityOr1(l.Opacity), 0, host)
		return
	}
	dc.SetColor(l.Color)
	dc.DrawRoundedRectangle(float64(x), float64(y), float64(w), float64(h), float64(l.Radius))
	dc.Fill()
}

// anchorOffset 返回覆盖图在 host 内按 anchor 对齐后的左上角坐标。
func anchorOffset(host image.Rectangle, w, h int, anchor string) (int, int) {
	hx, hy := host.Min.X, host.Min.Y
	hw, hh := host.Dx(), host.Dy()
	cx := hx + (hw-w)/2
	cy := hy + (hh-h)/2
	rx := hx + hw - w
	by := hy + hh - h
	switch anchor {
	case "top":
		return cx, hy
	case "top-right":
		return rx, hy
	case "left":
		return hx, cy
	case "center":
		return cx, cy
	case "right":
		return rx, cy
	case "bottom-left":
		return hx, by
	case "bottom":
		return cx, by
	case "bottom-right":
		return rx, by
	default: // top-left
		return hx, hy
	}
}

func opacityOr1(o float64) float64 {
	if o <= 0 {
		return 1
	}
	return o
}

func modeOrStretch(m string) string {
	if m == "" {
		return "stretch"
	}
	return m
}
