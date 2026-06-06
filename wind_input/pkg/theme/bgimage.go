package theme

import (
	"bytes"
	"encoding/base64"
	"fmt"
	"image"
	"image/draw"
	_ "image/jpeg"
	_ "image/png"
	"math"
	"os"
	"strings"

	xdraw "golang.org/x/image/draw"
)

// LoadBackgroundImage 从文件路径或 data: URI 加载图片为 *image.RGBA。
func LoadBackgroundImage(pathOrDataURI string) (*image.RGBA, error) {
	if pathOrDataURI == "" {
		return nil, fmt.Errorf("empty image path")
	}
	if strings.HasPrefix(pathOrDataURI, "data:") {
		return decodeDataURI(pathOrDataURI)
	}
	f, err := os.Open(pathOrDataURI)
	if err != nil {
		return nil, fmt.Errorf("open background image: %w", err)
	}
	defer f.Close()
	img, _, err := image.Decode(f)
	if err != nil {
		return nil, fmt.Errorf("decode background image: %w", err)
	}
	return toRGBA(img), nil
}

func decodeDataURI(uri string) (*image.RGBA, error) {
	// data:image/png;base64,xxxxx
	commaIdx := strings.IndexByte(uri, ',')
	if commaIdx < 0 {
		return nil, fmt.Errorf("invalid data URI: missing comma")
	}
	header := uri[:commaIdx]
	body := uri[commaIdx+1:]
	if !strings.Contains(header, ";base64") {
		return nil, fmt.Errorf("only base64-encoded data URIs supported")
	}
	data, err := base64.StdEncoding.DecodeString(body)
	if err != nil {
		return nil, fmt.Errorf("decode base64: %w", err)
	}
	img, _, err := image.Decode(bytes.NewReader(data))
	if err != nil {
		return nil, fmt.Errorf("decode data URI image: %w", err)
	}
	return toRGBA(img), nil
}

func toRGBA(img image.Image) *image.RGBA {
	if rgba, ok := img.(*image.RGBA); ok {
		return rgba
	}
	b := img.Bounds()
	rgba := image.NewRGBA(image.Rect(0, 0, b.Dx(), b.Dy()))
	draw.Draw(rgba, rgba.Bounds(), img, b.Min, draw.Src)
	return rgba
}

// DrawBackground 把 src 按 mode 绘制到 dst 的 rect 区域。
// mode: nine_slice | stretch | tile | center
// slice: 仅 nine_slice 用，描述源图四边边距像素值
// opacity: 0..1，作为整体 alpha 倍率应用到 src
// radius: 圆角半径（逻辑像素）。>0 时把 src 按 rect 的圆角矩形覆盖度遮罩——半径外的四角不绘制，
//
//	露出底层（如窗口背景）形成圆角。窗口靠外侧 alpha=0 也能裁角，但**内部元素**（如选中候选项，
//	四周已被窗口底色填成不透明）只能靠此遮罩裁圆角。radius=0 走快路径（整矩形，零回归）。
func DrawBackground(dst *image.RGBA, rect image.Rectangle, src *image.RGBA, mode string, slice Padding, opacity float64, radius int) {
	// clip=rect → 不额外裁剪（全覆盖老路，零回归）。
	DrawBackgroundClipped(dst, rect, src, mode, slice, opacity, radius, rect)
}

// DrawBackgroundClipped 同 DrawBackground，但额外把绘制硬裁到 clip 矩形内：clip 外的像素一律不画。
// 用于「定位图」（背景定位图 / 覆盖图层）——目标 rect 决定缩放/平铺，clip=host 边框盒保证不画到外面。
// clip 为空时退化为 rect（无额外裁剪）。注意 clip 只做矩形硬裁，圆角仍由 radius 经 clipRect/clipRad 处理。
func DrawBackgroundClipped(dst *image.RGBA, rect image.Rectangle, src *image.RGBA, mode string, slice Padding, opacity float64, radius int, clip image.Rectangle) {
	if src == nil || rect.Empty() {
		return
	}
	if opacity <= 0 {
		return
	}
	if opacity > 1 {
		opacity = 1
	}
	if clip.Empty() {
		clip = rect
	}
	rad := float64(radius)

	switch mode {
	case "nine_slice":
		drawNineSlice(dst, rect, src, slice, opacity, rad, clip)
	case "tile":
		drawTile(dst, rect, src, opacity, rad, clip)
	case "center":
		drawCenter(dst, rect, src, opacity, rad, clip)
	case "stretch":
		fallthrough
	default:
		drawStretch(dst, rect, src, opacity, rad, clip)
	}
}

func drawStretch(dst *image.RGBA, rect image.Rectangle, src *image.RGBA, opacity, rad float64, clip image.Rectangle) {
	scaled := scaleImage(src, rect.Dx(), rect.Dy())
	blendOver(dst, rect.Min, scaled, scaled.Bounds(), opacity, rect, rad, clip)
}

func drawCenter(dst *image.RGBA, rect image.Rectangle, src *image.RGBA, opacity, rad float64, clip image.Rectangle) {
	sb := src.Bounds()
	w, h := sb.Dx(), sb.Dy()
	x := rect.Min.X + (rect.Dx()-w)/2
	y := rect.Min.Y + (rect.Dy()-h)/2
	// 裁剪到 rect
	dstClip := image.Rect(x, y, x+w, y+h).Intersect(rect)
	if dstClip.Empty() {
		return
	}
	// 计算 src 中对应裁剪
	srcClip := image.Rect(
		sb.Min.X+(dstClip.Min.X-x),
		sb.Min.Y+(dstClip.Min.Y-y),
		sb.Min.X+(dstClip.Max.X-x),
		sb.Min.Y+(dstClip.Max.Y-y),
	)
	blendOver(dst, dstClip.Min, src, srcClip, opacity, rect, rad, clip)
}

func drawTile(dst *image.RGBA, rect image.Rectangle, src *image.RGBA, opacity, rad float64, clip image.Rectangle) {
	sb := src.Bounds()
	tw, th := sb.Dx(), sb.Dy()
	if tw == 0 || th == 0 {
		return
	}
	for y := rect.Min.Y; y < rect.Max.Y; y += th {
		for x := rect.Min.X; x < rect.Max.X; x += tw {
			tile := image.Rect(x, y, x+tw, y+th).Intersect(rect)
			if tile.Empty() {
				continue
			}
			srcClip := image.Rect(
				sb.Min.X,
				sb.Min.Y,
				sb.Min.X+tile.Dx(),
				sb.Min.Y+tile.Dy(),
			)
			blendOver(dst, tile.Min, src, srcClip, opacity, rect, rad, clip)
		}
	}
}

// drawNineSlice 把 src 按 slice 划分为 9 块：四角原样、四边在主轴方向拉伸、中心双轴拉伸
func drawNineSlice(dst *image.RGBA, rect image.Rectangle, src *image.RGBA, slice Padding, opacity, rad float64, clip image.Rectangle) {
	sb := src.Bounds()
	sw, sh := sb.Dx(), sb.Dy()

	// 边距夹紧避免越界
	clamp := func(v, max int) int {
		if v < 0 {
			return 0
		}
		if v > max {
			return max
		}
		return v
	}
	st := clamp(slice.Top, sh)
	sbt := clamp(slice.Bottom, sh)
	sl := clamp(slice.Left, sw)
	sr := clamp(slice.Right, sw)
	if st+sbt >= sh {
		st, sbt = sh/3, sh/3
	}
	if sl+sr >= sw {
		sl, sr = sw/3, sw/3
	}

	dw, dh := rect.Dx(), rect.Dy()
	dt := st
	dbt := sbt
	dl := sl
	dr := sr
	if dt+dbt > dh {
		ratio := float64(dh) / float64(dt+dbt)
		dt = int(float64(dt) * ratio)
		dbt = dh - dt
	}
	if dl+dr > dw {
		ratio := float64(dw) / float64(dl+dr)
		dl = int(float64(dl) * ratio)
		dr = dw - dl
	}

	// 9 区源/目标矩形
	type slice9 struct{ src, dst image.Rectangle }
	x0, y0 := sb.Min.X, sb.Min.Y
	X0, Y0 := rect.Min.X, rect.Min.Y
	slices := []slice9{
		// 上排：左上 / 上中 / 右上
		{image.Rect(x0, y0, x0+sl, y0+st), image.Rect(X0, Y0, X0+dl, Y0+dt)},
		{image.Rect(x0+sl, y0, x0+sw-sr, y0+st), image.Rect(X0+dl, Y0, X0+dw-dr, Y0+dt)},
		{image.Rect(x0+sw-sr, y0, x0+sw, y0+st), image.Rect(X0+dw-dr, Y0, X0+dw, Y0+dt)},
		// 中排：左中 / 中心 / 右中
		{image.Rect(x0, y0+st, x0+sl, y0+sh-sbt), image.Rect(X0, Y0+dt, X0+dl, Y0+dh-dbt)},
		{image.Rect(x0+sl, y0+st, x0+sw-sr, y0+sh-sbt), image.Rect(X0+dl, Y0+dt, X0+dw-dr, Y0+dh-dbt)},
		{image.Rect(x0+sw-sr, y0+st, x0+sw, y0+sh-sbt), image.Rect(X0+dw-dr, Y0+dt, X0+dw, Y0+dh-dbt)},
		// 下排：左下 / 下中 / 右下
		{image.Rect(x0, y0+sh-sbt, x0+sl, y0+sh), image.Rect(X0, Y0+dh-dbt, X0+dl, Y0+dh)},
		{image.Rect(x0+sl, y0+sh-sbt, x0+sw-sr, y0+sh), image.Rect(X0+dl, Y0+dh-dbt, X0+dw-dr, Y0+dh)},
		{image.Rect(x0+sw-sr, y0+sh-sbt, x0+sw, y0+sh), image.Rect(X0+dw-dr, Y0+dh-dbt, X0+dw, Y0+dh)},
	}
	for _, s := range slices {
		if s.src.Empty() || s.dst.Empty() {
			continue
		}
		// 角块尺寸相同直接 copy；否则缩放
		if s.src.Dx() == s.dst.Dx() && s.src.Dy() == s.dst.Dy() {
			blendOver(dst, s.dst.Min, src, s.src, opacity, rect, rad, clip)
		} else {
			scaled := scaleImageRect(src, s.src, s.dst.Dx(), s.dst.Dy())
			blendOver(dst, s.dst.Min, scaled, scaled.Bounds(), opacity, rect, rad, clip)
		}
	}
}

// scaleImage 把 src 缩放到 (w, h)
func scaleImage(src *image.RGBA, w, h int) *image.RGBA {
	return scaleImageRect(src, src.Bounds(), w, h)
}

// scaleImageRect 把 src 中的 srcRect 缩放到 (w, h)。
// 使用 golang.org/x/image/draw 的双线性插值（gg 等高质量绘图库的同款缩放器）：
//   - 在预乘 alpha 空间正确采样，半透明边缘不会因最近邻离散采样而毛糙；
//   - 无 CatmullRom 那样的过冲振铃，避免在硬透明/不透明边界引入光晕。
//
// 输出仍为预乘 alpha 的 *image.RGBA，交给 blendOver 在预乘空间合成。
func scaleImageRect(src *image.RGBA, srcRect image.Rectangle, w, h int) *image.RGBA {
	dst := image.NewRGBA(image.Rect(0, 0, w, h))
	if srcRect.Dx() == 0 || srcRect.Dy() == 0 || w == 0 || h == 0 {
		return dst
	}
	xdraw.BiLinear.Scale(dst, dst.Bounds(), src, srcRect, xdraw.Src, nil)
	return dst
}

// blendOver 以 opacity 倍率把 src 的 srcRect 区域 over-blend 到 dst 的 dstMin 起点。
//
// **预乘 alpha 合成**：src/dst 均为 Go 标准 *image.RGBA，其 Pix 是预乘（premultiplied）的
// （toRGBA 经 draw.Draw 转换时已预乘，x/image 缩放输出亦为预乘）。因此用预乘 over operator：
//
//	out = src + dst*(1-src_a)
//
// 早期代码误把预乘 RGB 当作 straight，再乘一次 src_a，导致半透明边缘被二次衰减发暗
// （实心内部不受影响 → 视觉上多出一圈暗边）。改为预乘合成后边缘正确羽化、无暗环。
//
// 圆角裁剪（两道，互补）：
//   - **dst alpha 遮罩**：dst alpha=0 跳过、其余按 dA/255 缩放 src 贡献。这裁掉窗口**外侧**
//     （alpha=0）的四角，对最外层窗口够用。
//   - **clipRad 圆角覆盖遮罩**：clipRect+clipRad>0 时，按圆角矩形覆盖度再缩放 src 贡献。这是
//     **内部元素**（如选中候选项，四周已被窗口底色填成 alpha=255，dst 遮罩失效）裁圆角的唯一手段：
//     半径外四角 coverage=0 → 不画 → 露出底层窗口背景。clipRad<=0 时整矩形绘制（零回归）。
//
// clipBounds：矩形硬裁——dst 中落在此矩形外的像素一律不画（定位图裁到 host 边框盒、不外溢）。
// 全覆盖路径传 clipBounds=rect 自身 → 恒含所有绘制像素，no-op（零回归）。
func blendOver(dst *image.RGBA, dstMin image.Point, src *image.RGBA, srcRect image.Rectangle, opacity float64, clipRect image.Rectangle, clipRad float64, clipBounds image.Rectangle) {
	if opacity <= 0 {
		return
	}
	alphaMul := uint32(opacity*255 + 0.5)

	dx0, dy0 := dstMin.X, dstMin.Y
	sx0, sy0 := srcRect.Min.X, srcRect.Min.Y
	w, h := srcRect.Dx(), srcRect.Dy()

	dstBounds := dst.Bounds()
	srcBounds := src.Bounds()
	for y := 0; y < h; y++ {
		dy := dy0 + y
		if dy < dstBounds.Min.Y || dy >= dstBounds.Max.Y {
			continue
		}
		if dy < clipBounds.Min.Y || dy >= clipBounds.Max.Y { // 矩形硬裁（上下）
			continue
		}
		sy := sy0 + y
		if sy < srcBounds.Min.Y || sy >= srcBounds.Max.Y {
			continue
		}
		for x := 0; x < w; x++ {
			dx := dx0 + x
			if dx < dstBounds.Min.X || dx >= dstBounds.Max.X {
				continue
			}
			if dx < clipBounds.Min.X || dx >= clipBounds.Max.X { // 矩形硬裁（左右）
				continue
			}
			sx := sx0 + x
			if sx < srcBounds.Min.X || sx >= srcBounds.Max.X {
				continue
			}

			dOff := (dy-dstBounds.Min.Y)*dst.Stride + (dx-dstBounds.Min.X)*4
			dA := uint32(dst.Pix[dOff+3])
			// 圆角保护：dst 完全透明的像素不绘制
			if dA == 0 {
				continue
			}
			// 圆角覆盖遮罩（内部元素裁角）：clipRad<=0 时 covM=255（整矩形，零回归）
			covM := uint32(255)
			if clipRad > 0 {
				cov := roundedCoverage(dx, dy, clipRect, clipRad)
				if cov <= 0 {
					continue
				}
				covM = uint32(cov*255 + 0.5)
			}

			sOff := (sy-srcBounds.Min.Y)*src.Stride + (sx-srcBounds.Min.X)*4
			// 预乘 src × 整体 opacity，再用 dst 覆盖度 + 圆角覆盖度遮罩
			sR := uint32(src.Pix[sOff]) * alphaMul / 255 * dA / 255 * covM / 255
			sG := uint32(src.Pix[sOff+1]) * alphaMul / 255 * dA / 255 * covM / 255
			sB := uint32(src.Pix[sOff+2]) * alphaMul / 255 * dA / 255 * covM / 255
			sA := uint32(src.Pix[sOff+3]) * alphaMul / 255 * dA / 255 * covM / 255

			dR := uint32(dst.Pix[dOff])
			dG := uint32(dst.Pix[dOff+1])
			dB := uint32(dst.Pix[dOff+2])

			inv := 255 - sA
			dst.Pix[dOff] = uint8(sR + dR*inv/255)
			dst.Pix[dOff+1] = uint8(sG + dG*inv/255)
			dst.Pix[dOff+2] = uint8(sB + dB*inv/255)
			dst.Pix[dOff+3] = uint8(sA + dA*inv/255)
		}
	}
}

// roundedCoverage 返回像素 (px,py) 相对 rect 圆角矩形的覆盖度 [0,1]：
// 圆角弧内=1、弧外=0、弧边 1px 线性抗锯齿；非角区恒 1。rad<=0 由调用方短路（不进此函数）。
func roundedCoverage(px, py int, rect image.Rectangle, rad float64) float64 {
	fx, fy := float64(px)+0.5, float64(py)+0.5
	minX, minY := float64(rect.Min.X), float64(rect.Min.Y)
	maxX, maxY := float64(rect.Max.X), float64(rect.Max.Y)
	var cx, cy float64 // 当前像素所属角的圆心
	switch {
	case fx < minX+rad && fy < minY+rad:
		cx, cy = minX+rad, minY+rad // 左上
	case fx > maxX-rad && fy < minY+rad:
		cx, cy = maxX-rad, minY+rad // 右上
	case fx < minX+rad && fy > maxY-rad:
		cx, cy = minX+rad, maxY-rad // 左下
	case fx > maxX-rad && fy > maxY-rad:
		cx, cy = maxX-rad, maxY-rad // 右下
	default:
		return 1 // 非角区：整覆盖
	}
	d := math.Hypot(fx-cx, fy-cy)
	if d <= rad-0.5 {
		return 1
	}
	if d >= rad+0.5 {
		return 0
	}
	return rad + 0.5 - d // 1px 线性抗锯齿带
}
