package ui

import (
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
// （来自各自 ResolvedV25.Resources）——因为 resources 的权威来源是各窗口的 resolvedV25
// （候选窗测试亦直接写 resolvedV25 而不经 SetTheme）；imageResolver 只持解码缓存，
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

// fillFor 构建 View 背景填充：底色 + 可选背景图（按 RVImage spec 经 imageForRef 取缓存位图）。
// bg 为 nil 或图解码失败时退化为纯底色（零回归）。
func (ir *imageResolver) fillFor(col color.Color, bg *theme.RVImage, resources map[string]string) Fill {
	f := Fill{Color: col}
	if bg != nil {
		if img := ir.imageForRef(bg.Ref, resources); img != nil {
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
		img := ir.imageForRef(L.Ref, resources)
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
			W:       sc(float64(L.W)),
			H:       sc(float64(L.H)),
		})
	}
}
