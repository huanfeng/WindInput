//go:build windows

package ui

import (
	"image"
	"image/color"
	"log/slog"
	"sync"

	"github.com/gogpu/gg"
	"github.com/huanfeng/wind_input/pkg/theme"
)

// ToastLevel 决定 toast 配色，呼应消息严重程度。
// ToastLevel / ToastPosition / ToastOptions 已迁至 types_neutral.go (平台无关)。

// ToastRenderer 负责把 ToastOptions 渲染成 RGBA 图像。复用 TextBackendManager 的 DirectWrite 后端，
// 与 tooltip / status 渲染保持一致的反锯齿表现。
type ToastRenderer struct {
	TextBackendManager

	mu            sync.Mutex
	resolvedTheme *theme.ResolvedTheme
	logger        *slog.Logger
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
func (r *ToastRenderer) SetTheme(resolved *theme.ResolvedTheme) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.resolvedTheme = resolved
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

// getColors 返回背景 + 正文文本颜色。Toast 一律不透明（与系统通知一致，避免重要信息看不清）。
func (r *ToastRenderer) getColors() (bg, text color.Color) {
	r.mu.Lock()
	resolved := r.resolvedTheme
	r.mu.Unlock()

	bg = color.RGBA{R: 0x2B, G: 0x2B, B: 0x2B, A: 0xFF}
	text = color.RGBA{R: 0xFF, G: 0xFF, B: 0xFF, A: 0xFF}
	if resolved != nil {
		// Tooltip 调色板与 toast 语义最接近：暗背景 + 浅文本。
		bg = forceAlphaOpaque(resolved.Tooltip.BackgroundColor)
		text = resolved.Tooltip.TextColor
	}
	return bg, text
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
	titleSize := 15.0 * scale
	bodySize := 13.0 * scale
	padding := 12.0 * scale
	lineSpacing := 4.0 * scale
	titleGap := 6.0 * scale // 标题与正文之间额外间距
	borderRadius := 6.0 * scale
	// 左侧 accent 条参数：完全位于不透明背景内部, 不与圆角外缘相切, 避免反锯齿像素溢出到
	// layered 窗口的透明区域产生"边缘透色"问题。
	accentBarWidth := 4.0 * scale
	accentBarInset := 5.0 * scale // accent 条距 bg 左侧 / 上下圆角的安全距离
	// 文本左侧留白需绕开 accent 条 + 一小段呼吸空间。
	textLeft := accentBarInset + accentBarWidth + 8.0*scale

	r.mu.Lock()
	td := r.TextDrawer()
	r.mu.Unlock()

	bg, textColor := r.getColors()
	accent := levelAccent(opts.Level)

	// 计算可用内容宽度（左 textLeft + 右 padding 之间的可绘区域）。
	var innerMax float64
	if maxContentPx > 0 {
		innerMax = float64(maxContentPx) - textLeft - padding
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

	width := contentWidth + textLeft + padding
	if width < 160*scale {
		width = 160 * scale // 太窄不好看
	}

	// 计算总高度：title 行高 + titleGap + 正文 N 行 + lineSpacing 间距。
	var height float64 = padding * 2
	if title != "" {
		height += titleSize
		if len(bodyLines) > 0 {
			height += titleGap
		}
	}
	if len(bodyLines) > 0 {
		height += bodySize*float64(len(bodyLines)) + lineSpacing*float64(len(bodyLines)-1)
	}

	dc := gg.NewContext(int(width), int(height))

	// 1. 圆角背景（唯一与外侧透明区相邻的图层；它的反锯齿是 bg 色，融入桌面观感干净）
	dc.SetColor(bg)
	dc.DrawRoundedRectangle(0, 0, width, height, borderRadius)
	dc.Fill()

	// 2. 左侧 accent 条：用 Fill 而非 Stroke, 全部位于 bg 内部, 不接触圆角外缘 → 反锯齿
	// 像素只与不透明 bg 叠加, 不会有"边缘透出底色"的观感。
	barX := accentBarInset
	barY := accentBarInset
	barH := height - accentBarInset*2
	if barH > 0 {
		dc.SetColor(accent)
		dc.DrawRoundedRectangle(barX, barY, accentBarWidth, barH, accentBarWidth/2)
		dc.Fill()
	}

	img := dc.Image().(*image.RGBA)
	td.BeginDraw(img)

	y := padding
	if title != "" {
		// 标题用 accent 颜色，醒目；基线偏移 ≈ size * 0.8
		td.DrawString(title, textLeft, y+titleSize*0.8, titleSize, accent)
		y += titleSize + titleGap
	}
	for i, line := range bodyLines {
		baseline := y + bodySize*0.8 + float64(i)*(bodySize+lineSpacing)
		td.DrawString(line, textLeft, baseline, bodySize, textColor)
	}
	td.EndDraw()

	DrawDebugBanner(img)
	return img
}
