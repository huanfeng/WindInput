package theme

// gradient.go — 渐变（linear/radial）解析与栅格化（P7-E 落地）。
//
// resolveGradient：ViewGradient（schema）→ RVGradient（消费形态：stop 颜色解析 + 按 Pos 排序）。
// RasterizeGradient：RVGradient → 预乘 alpha 位图，由 ui paintShapes 按目标 rect 现场栅格化，
// 复用 DrawBackground 的圆角裁剪 + 预乘合成（与 RasterizeSVG 同约定，单一收口便于换实现）。

import (
	"image"
	"image/color"
	"math"
	"sort"
)

// resolveGradient 把 schema ViewGradient 解析为渲染消费的 RVGradient：
// stop 颜色经 resolveColor 解析（nil 跳过），Pos 钳到 [0,1] 后按升序排序。无有效 stop 返回 nil。
func resolveGradient(g *ViewGradient, resolveColor func(ColorRef) color.Color) *RVGradient {
	if g == nil {
		return nil
	}
	stops := make([]RVGradientStop, 0, len(g.Stops))
	for _, s := range g.Stops {
		c := resolveColor(s.Color)
		if c == nil {
			continue
		}
		stops = append(stops, RVGradientStop{Color: c, Pos: clampGradPos(s.Pos)})
	}
	if len(stops) == 0 {
		return nil
	}
	sort.SliceStable(stops, func(i, j int) bool { return stops[i].Pos < stops[j].Pos })
	return &RVGradient{Type: g.Type, Angle: g.Angle, Stops: stops}
}

func clampGradPos(p float64) float64 {
	if p < 0 {
		return 0
	}
	if p > 1 {
		return 1
	}
	return p
}

// RasterizeGradient 把渐变按目标设备像素 w×h 栅格化为预乘 alpha 的 *image.RGBA。
//   - linear：方向向量 (cosθ, sinθ)（θ=Angle°，图像 y 向下；0=左→右、90=上→下），
//     沿矩形四角投影范围归一化到 [0,1] 插值。
//   - radial：圆心=矩形中心，按宽/高半轴归一化的椭圆距离，中心 t=0 → 最远角 t=1。
//
// 无效 spec（nil / 无 stop / 尺寸非正）返回 nil。
func RasterizeGradient(g *RVGradient, w, h int) *image.RGBA {
	if g == nil || w <= 0 || h <= 0 || len(g.Stops) == 0 {
		return nil
	}
	rgba := image.NewRGBA(image.Rect(0, 0, w, h))
	stops := g.Stops
	radial := g.Type == "radial"

	fw, fh := float64(w), float64(h)
	cx, cy := fw/2, fh/2

	var dx, dy, projMin, projLen float64
	if !radial {
		rad := g.Angle * math.Pi / 180
		dx, dy = math.Cos(rad), math.Sin(rad)
		projMin = math.Inf(1)
		projMax := math.Inf(-1)
		for _, c := range [4][2]float64{{0, 0}, {fw, 0}, {0, fh}, {fw, fh}} {
			p := c[0]*dx + c[1]*dy
			if p < projMin {
				projMin = p
			}
			if p > projMax {
				projMax = p
			}
		}
		projLen = projMax - projMin
		if projLen == 0 {
			projLen = 1
		}
	}

	for py := 0; py < h; py++ {
		for px := 0; px < w; px++ {
			fx, fy := float64(px)+0.5, float64(py)+0.5
			var t float64
			if radial {
				ndx, ndy := 0.0, 0.0
				if cx > 0 {
					ndx = (fx - cx) / cx
				}
				if cy > 0 {
					ndy = (fy - cy) / cy
				}
				t = math.Hypot(ndx, ndy) / math.Sqrt2 // 最远角 = 1
			} else {
				t = ((fx*dx + fy*dy) - projMin) / projLen
			}
			r, gn, b, a := lerpStops(stops, clampGradPos(t))
			off := rgba.PixOffset(px, py)
			// 预乘 alpha 写入（DrawBackground/blendOver 在预乘空间合成）。
			rgba.Pix[off+0] = premulGrad(r, a)
			rgba.Pix[off+1] = premulGrad(gn, a)
			rgba.Pix[off+2] = premulGrad(b, a)
			rgba.Pix[off+3] = a
		}
	}
	return rgba
}

// straightGradRGBA 取颜色的 straight（非预乘）8 位分量：项目颜色多为 color.RGBA（straight 语义存储），
// 直接取分量；其它实现回退 RGBA()（预乘 16 位）降采样。
func straightGradRGBA(c color.Color) (r, g, b, a uint8) {
	if rc, ok := c.(color.RGBA); ok {
		return rc.R, rc.G, rc.B, rc.A
	}
	r16, g16, b16, a16 := c.RGBA()
	return uint8(r16 >> 8), uint8(g16 >> 8), uint8(b16 >> 8), uint8(a16 >> 8)
}

// lerpStops 在已排序 stops 上按 t∈[0,1] 线性插值（straight 空间），返回 straight 8 位分量。
func lerpStops(stops []RVGradientStop, t float64) (r, g, b, a uint8) {
	n := len(stops)
	if t <= stops[0].Pos {
		return straightGradRGBA(stops[0].Color)
	}
	if t >= stops[n-1].Pos {
		return straightGradRGBA(stops[n-1].Color)
	}
	for i := 1; i < n; i++ {
		if t <= stops[i].Pos {
			s1, s2 := stops[i-1], stops[i]
			lt := 0.0
			if span := s2.Pos - s1.Pos; span > 0 {
				lt = (t - s1.Pos) / span
			}
			r1, g1, b1, a1 := straightGradRGBA(s1.Color)
			r2, g2, b2, a2 := straightGradRGBA(s2.Color)
			return lerpU8(r1, r2, lt), lerpU8(g1, g2, lt), lerpU8(b1, b2, lt), lerpU8(a1, a2, lt)
		}
	}
	return straightGradRGBA(stops[n-1].Color)
}

func lerpU8(a, b uint8, t float64) uint8 {
	return uint8(float64(a) + (float64(b)-float64(a))*t + 0.5)
}

func premulGrad(c, a uint8) uint8 {
	return uint8(uint32(c) * uint32(a) / 255)
}
