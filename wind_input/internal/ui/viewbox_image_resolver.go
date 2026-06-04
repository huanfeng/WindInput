package ui

import (
	"fmt"
	"image"
	"image/color"
	"sync"

	"github.com/huanfeng/wind_input/pkg/theme"
)

// imageResolver 是候选窗与其它独立窗口（status/tooltip/menu/toast）共享的位图解码缓存基础设施
// （P8 切片6）：把 ViewImage.ref 解码为位图并按 ref 缓存（含 nil 失败结果，避免每帧重试）。
// 从候选窗 Renderer 的 imageForRef/fillFor/appendThemeLayers 抽出（原 P7-C 实现）。
//
// resources 表（ref→path/dataURI）不由 imageResolver 持有，而是各调用方按帧传入
// （来自各自 ResolvedV3.Resources）——因为 resources 的权威来源是各窗口的 resolvedV3
// （候选窗测试亦直接写 resolvedV3 而不经 SetTheme）；imageResolver 只持解码缓存，
// 换主题时由调用方 SetTheme 触发 reset() 清缓存（ref 解码结果按主题失效）。
//
// 自带 mutex 使其并发安全：候选窗单线程使用（无竞争，锁近乎零成本），其它窗口的
// Render 可能与 SetTheme 不同线程，共享同一把锁即可避免 cache 读写竞争。
type imageResolver struct {
	mu    sync.Mutex
	cache map[string]*image.RGBA
}

// reset 清空解码缓存（换主题时调用）。
func (ir *imageResolver) reset() {
	ir.mu.Lock()
	ir.cache = nil
	ir.mu.Unlock()
}

// imageForRef 把 ViewImage.ref 解码为位图：先查缓存，未命中则解析 ref（resources 表 →
// 字面 path/data URI）并一次性解码后缓存（含失败的 nil，避免每帧重试）。
func (ir *imageResolver) imageForRef(ref string, resources map[string]string) *image.RGBA {
	if ref == "" {
		return nil
	}
	ir.mu.Lock()
	defer ir.mu.Unlock()
	if ir.cache == nil {
		ir.cache = make(map[string]*image.RGBA)
	}
	if img, ok := ir.cache[ref]; ok {
		return img
	}
	path := ref
	if resources != nil {
		if p, ok := resources[ref]; ok {
			path = p
		}
	}
	img, err := theme.LoadBackgroundImage(path)
	if err != nil {
		img = nil // 缓存失败结果，不再重试（不打断渲染）
	}
	ir.cache[ref] = img
	return img
}

// resolveImage 取图（统一入口，支持矢量 + 单色染色）：
//   - SVG ref：按目标 w×h 现场栅格化（缓存键含尺寸）；w/h<=0（动态背景等）兜底 64×64 由后续 gg 缩放。
//   - 位图 ref：尺寸无关解码（与 imageForRef 同 ref 键共享缓存）。
//   - tint 非 nil：把图当 alpha mask、用 tint 色填充（单色图标随主题变色），缓存键含色值。
//
// 解码/栅格化/染色结果一律缓存（含失败 nil，避免每帧重试）。
func (ir *imageResolver) resolveImage(ref string, resources map[string]string, w, h int, tint color.Color) *image.RGBA {
	if ref == "" {
		return nil
	}
	path := ref
	if resources != nil {
		if p, ok := resources[ref]; ok {
			path = p
		}
	}
	isSVG := theme.IsSVGRef(path)
	tw, th := w, h
	if isSVG && (tw <= 0 || th <= 0) {
		tw, th = 64, 64 // 动态尺寸（如背景）兜底分辨率
	}
	key := ref
	if isSVG {
		key = fmt.Sprintf("%s@%dx%d", ref, tw, th)
	}
	if tint != nil {
		r16, g16, b16, a16 := tint.RGBA()
		key = fmt.Sprintf("%s#%02x%02x%02x%02x", key, r16>>8, g16>>8, b16>>8, a16>>8)
	}
	ir.mu.Lock()
	defer ir.mu.Unlock()
	if ir.cache == nil {
		ir.cache = make(map[string]*image.RGBA)
	}
	if img, ok := ir.cache[key]; ok {
		return img
	}
	var img *image.RGBA
	var err error
	if isSVG {
		img, err = theme.RasterizeSVG(path, tw, th)
	} else {
		img, err = theme.LoadBackgroundImage(path)
	}
	if err != nil {
		img = nil
	}
	if img != nil && tint != nil {
		img = tintMask(img, tint)
	}
	ir.cache[key] = img
	return img
}

// tintMask 把图当 alpha mask、用 c 填充（预乘 alpha）：保留每像素 alpha，RGB 换成 c。
// 用于单色图标随主题变色（SVG/PNG 出形状，颜色跟主题）。
func tintMask(src *image.RGBA, c color.Color) *image.RGBA {
	r16, g16, b16, _ := c.RGBA()
	cr, cg, cb := r16>>8, g16>>8, b16>>8
	b := src.Bounds()
	out := image.NewRGBA(b)
	for y := b.Min.Y; y < b.Max.Y; y++ {
		so := src.PixOffset(b.Min.X, y)
		do := out.PixOffset(b.Min.X, y)
		for x := 0; x < b.Dx(); x++ {
			a := uint32(src.Pix[so+3])
			out.Pix[do+0] = uint8(cr * a / 255)
			out.Pix[do+1] = uint8(cg * a / 255)
			out.Pix[do+2] = uint8(cb * a / 255)
			out.Pix[do+3] = uint8(a)
			so += 4
			do += 4
		}
	}
	return out
}

// fillFor 构建 View 背景填充：底色 + 可选背景图（经 resolveImage 取缓存位图，支持 SVG/tint）。
// bg 为 nil 或图解码失败时退化为纯底色（零回归）。背景尺寸动态 → SVG 兜底分辨率后 gg 缩放。
func (ir *imageResolver) fillFor(col color.Color, bg *theme.RVImage, resources map[string]string) Fill {
	f := Fill{Color: col}
	if bg != nil {
		if img := ir.resolveImage(bg.Ref, resources, 0, 0, bg.TintColor); img != nil {
			f.Image = img
			f.Mode = bg.Mode
			f.Slice = bg.Slice
			f.Opacity = bg.Opacity
		}
	}
	return f
}

// appendLayers 把主题 RVImage 层级覆盖图（spec）解码后追加到 View.Layers。
// offset/size 为逻辑像素经 sc 缩放，W/H=0 保持原图尺寸；解码失败的层静默跳过（不打断渲染）。
func (ir *imageResolver) appendLayers(v *View, layers []theme.RVImage, resources map[string]string, sc func(float64) int) {
	for i := range layers {
		L := &layers[i]
		w, h := sc(float64(L.W)), sc(float64(L.H))
		img := ir.resolveImage(L.Ref, resources, w, h, L.TintColor) // SVG 按层尺寸栅格化、可染色
		if img == nil {
			continue
		}
		v.Layers = append(v.Layers, ImageLayer{
			Img:     img,
			Mode:    L.Mode,
			Slice:   L.Slice,
			Opacity: L.Opacity,
			Z:       L.Z,
			Anchor:  L.Anchor,
			OffsetX: sc(float64(L.OffsetX)),
			OffsetY: sc(float64(L.OffsetY)),
			W:       w,
			H:       h,
		})
	}
}
