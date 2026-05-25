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
// Info/Success/Warn/Error 仅改变边框 + accent，背景统一沿用 tooltip 主题色，避免割裂。
type ToastLevel int

const (
	ToastInfo    ToastLevel = iota // 蓝色 accent: 普通提示
	ToastSuccess                   // 绿色 accent: 操作成功 / 资源就绪
	ToastWarn                      // 橙色 accent: 需要注意
	ToastError                     // 红色 accent: 操作失败
)

// ToastPosition 决定 toast 在屏幕上的落位策略。
type ToastPosition int

const (
	ToastCenter      ToastPosition = iota // 屏幕（工作区）正中
	ToastBottomRight                      // 工作区右下角
	ToastTopRight                         // 预留：工作区右上角
	ToastTop                              // 预留：工作区顶部居中
)

// ToastOptions 描述一次 toast 展示请求。空字段使用 toastWindow 内部默认值。
type ToastOptions struct {
	Title    string        // 可选：第一行加粗大字
	Message  string        // 主体文本，支持 "\n" 换行
	Level    ToastLevel    // 默认 ToastInfo
	Position ToastPosition // 默认 ToastBottomRight
	Duration int           // 自动隐藏毫秒数；0=用默认 5000；<0=不自动隐藏
	MaxWidth int           // 内容最大像素宽（DIP）；0=使用工作区一半作为上限
}

// ToastRenderer 负责把 ToastOptions 渲染成 RGBA 图像。复用 TextBackendManager 的 DirectWrite 后端，
// 与 tooltip / status 渲染保持一致的反锯齿表现。
type ToastRenderer struct {
	TextBackendManager

	mu            sync.Mutex
	resolvedTheme *theme.ResolvedTheme
	logger        *slog.Logger
}

// NewToastRenderer 创建 toast 渲染器。
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
func levelAccent(level ToastLevel) color.Color {
	switch level {
	case ToastSuccess:
		return color.RGBA{R: 0x4C, G: 0xAF, B: 0x50, A: 0xFF} // 绿
	case ToastWarn:
		return color.RGBA{R: 0xFF, G: 0x98, B: 0x00, A: 0xFF} // 琥珀
	case ToastError:
		return color.RGBA{R: 0xE5, G: 0x39, B: 0x35, A: 0xFF} // 红
	case ToastInfo:
		fallthrough
	default:
		return color.RGBA{R: 0x42, G: 0xA5, B: 0xF5, A: 0xFF} // 蓝
	}
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
	borderWidth := 2.0 * scale

	r.mu.Lock()
	td := r.TextDrawer()
	r.mu.Unlock()

	bg, textColor := r.getColors()
	accent := levelAccent(opts.Level)

	// 计算可用内容宽度（不含 padding）。
	var innerMax float64
	if maxContentPx > 0 {
		innerMax = float64(maxContentPx) - padding*2
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

	width := contentWidth + padding*2
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

	// 1. 圆角背景
	dc.SetColor(bg)
	dc.DrawRoundedRectangle(0, 0, width, height, borderRadius)
	dc.Fill()

	// 2. accent 边框（绘制在背景之上，沿圆角矩形勾线）
	dc.SetColor(accent)
	dc.SetLineWidth(borderWidth)
	dc.DrawRoundedRectangle(borderWidth/2, borderWidth/2, width-borderWidth, height-borderWidth, borderRadius)
	dc.Stroke()

	img := dc.Image().(*image.RGBA)
	td.BeginDraw(img)

	y := padding
	if title != "" {
		// 标题用 accent 颜色，醒目；基线偏移 ≈ size * 0.8
		td.DrawString(title, padding, y+titleSize*0.8, titleSize, accent)
		y += titleSize + titleGap
	}
	for i, line := range bodyLines {
		baseline := y + bodySize*0.8 + float64(i)*(bodySize+lineSpacing)
		td.DrawString(line, padding, baseline, bodySize, textColor)
	}
	td.EndDraw()

	DrawDebugBanner(img)
	return img
}
