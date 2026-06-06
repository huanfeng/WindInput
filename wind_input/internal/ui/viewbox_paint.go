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
	"image/color"
	"math"

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
		if v.Shadow.Blur > 0 || v.Shadow.Spread > 0 {
			paintBlurredShadow(img, v, x, y, w, h, rad)
		} else {
			dc.SetColor(v.Shadow.Color)
			dc.DrawRoundedRectangle(x+float64(v.Shadow.OffsetX), y+float64(v.Shadow.OffsetY), w, h, rad)
			dc.Fill()
		}
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

// paintBlurredShadow 绘制带 blur/spread 的模糊投影到 dst。
// ① 在临时画布上绘制 spread 扩散后的实心圆角矩形；② alpha 通道做 3 次方框模糊（逼近高斯）；
// ③ 以 shadow.Color 着色后 src-over 合成到 dst（dst 存储预乘 alpha）。
func paintBlurredShadow(dst *image.RGBA, v *View, x, y, w, h, borderRadius float64) {
	sh := v.Shadow
	spread := float64(sh.Spread)
	blur := sh.Blur
	offsetX := float64(sh.OffsetX)
	offsetY := float64(sh.OffsetY)

	// 扩散后阴影盒在 canvas 坐标中的位置与尺寸
	boxW := w + 2*spread
	boxH := h + 2*spread
	boxX := x + offsetX - spread
	boxY := y + offsetY - spread

	// 临时图：阴影盒 + 四周足够的 padding 容纳模糊梯度。
	// 3 次方框模糊级联后 sigma ≈ sqrt(blur*(blur+2))，3-sigma 需约 3×sigma px 才能衰减到透明。
	// pad = ceil(3×sigma) + 2（AA 裕量），远比 blur+2 宽裕。
	sigma := math.Sqrt(float64(blur) * float64(blur+2))
	pad := int(math.Ceil(3*sigma)) + 2
	tmpW := int(math.Ceil(boxW)) + 2*pad
	tmpH := int(math.Ceil(boxH)) + 2*pad
	if tmpW < 1 {
		tmpW = 1
	}
	if tmpH < 1 {
		tmpH = 1
	}

	// 临时图中阴影盒左上角（保留亚像素偏移以维持 AA 精度）
	localX := float64(pad) + (boxX - math.Floor(boxX))
	localY := float64(pad) + (boxY - math.Floor(boxY))

	buf := make([]byte, tmpW*tmpH*4)
	pm := gg.NewPixmapFromBuffer(buf, tmpW, tmpH)
	tmpDc := gg.NewContextForPixmap(pm)
	tmpImg := pm.ImageView()

	tmpDc.SetColor(color.RGBA{0, 0, 0, 255})
	tmpDc.DrawRoundedRectangle(localX, localY, boxW, boxH, borderRadius)
	tmpDc.Fill()

	// 3 次方框模糊 alpha ≈ 高斯模糊
	for i := 0; i < 3; i++ {
		boxBlurAlpha(tmpImg, blur)
	}

	// color.RGBA.RGBA() 返回非预乘 16-bit（>>8 = 原字节值）
	sr32, sg32, sb32, sa32 := sh.Color.RGBA()
	shadowR := uint32(sr32 >> 8)
	shadowG := uint32(sg32 >> 8)
	shadowB := uint32(sb32 >> 8)
	shadowA := uint32(sa32 >> 8)

	// 临时图左上角在 dst 中的整像素起点
	dstX0 := int(math.Floor(boxX)) - pad
	dstY0 := int(math.Floor(boxY)) - pad
	dstBounds := dst.Bounds()

	for ty := 0; ty < tmpH; ty++ {
		for tx := 0; tx < tmpW; tx++ {
			maskA := uint32(tmpImg.Pix[ty*tmpImg.Stride+tx*4+3])
			if maskA == 0 {
				continue
			}
			finalA := maskA * shadowA / 255
			if finalA == 0 {
				continue
			}
			dx := dstX0 + tx
			dy := dstY0 + ty
			if dx < dstBounds.Min.X || dx >= dstBounds.Max.X || dy < dstBounds.Min.Y || dy >= dstBounds.Max.Y {
				continue
			}
			// 预乘源颜色，src-over 合成
			srcR := shadowR * finalA / 255
			srcG := shadowG * finalA / 255
			srcB := shadowB * finalA / 255
			srcA := finalA
			off := dst.PixOffset(dx, dy)
			inv := 255 - srcA
			dst.Pix[off+0] = uint8((srcR*255 + uint32(dst.Pix[off+0])*inv) / 255)
			dst.Pix[off+1] = uint8((srcG*255 + uint32(dst.Pix[off+1])*inv) / 255)
			dst.Pix[off+2] = uint8((srcB*255 + uint32(dst.Pix[off+2])*inv) / 255)
			dst.Pix[off+3] = uint8((srcA*255 + uint32(dst.Pix[off+3])*inv) / 255)
		}
	}
}

// boxBlurAlpha 对 img alpha 通道做一次可分离方框模糊（水平 + 垂直，O(w×h) 与 r 无关）。
// 边界使用延伸（clamp）模式；三次调用可逼近高斯模糊。
func boxBlurAlpha(img *image.RGBA, r int) {
	if r <= 0 {
		return
	}
	w := img.Bounds().Dx()
	h := img.Bounds().Dy()
	if w == 0 || h == 0 {
		return
	}
	diam := 2*r + 1
	tmp := make([]uint8, w*h)

	// 水平方向滑动窗口
	for row := 0; row < h; row++ {
		var sum int
		for i := -r; i <= r; i++ {
			xi := clampInt(i, 0, w-1)
			sum += int(img.Pix[row*img.Stride+xi*4+3])
		}
		for col := 0; col < w; col++ {
			tmp[row*w+col] = uint8(sum / diam)
			removeX := clampInt(col-r, 0, w-1)
			addX := clampInt(col+r+1, 0, w-1)
			sum += int(img.Pix[row*img.Stride+addX*4+3]) - int(img.Pix[row*img.Stride+removeX*4+3])
		}
	}

	// 垂直方向滑动窗口，tmp → img alpha
	for col := 0; col < w; col++ {
		var sum int
		for i := -r; i <= r; i++ {
			yi := clampInt(i, 0, h-1)
			sum += int(tmp[yi*w+col])
		}
		for row := 0; row < h; row++ {
			img.Pix[row*img.Stride+col*4+3] = uint8(sum / diam)
			removeY := clampInt(row-r, 0, h-1)
			addY := clampInt(row+r+1, 0, h-1)
			sum += int(tmp[addY*w+col]) - int(tmp[removeY*w+col])
		}
	}
}

func clampInt(v, lo, hi int) int {
	if v < lo {
		return lo
	}
	if v > hi {
		return hi
	}
	return v
}
