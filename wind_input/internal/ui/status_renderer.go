package ui

import (
	"image"
	"image/color"
	"log/slog"
	"strings"
	"sync"

	"github.com/gogpu/gg"
	"github.com/huanfeng/wind_input/pkg/theme"
)

// StatusRenderer 负责将状态信息渲染为图像
type StatusRenderer struct {
	TextBackendManager

	mu            sync.Mutex
	resolvedTheme *theme.ResolvedTheme
	logger        *slog.Logger
}

// NewStatusRenderer 创建状态渲染器，默认使用 DirectWrite 渲染（与系统默认一致，反锯齿效果更好）
func NewStatusRenderer(logger *slog.Logger) *StatusRenderer {
	r := &StatusRenderer{
		TextBackendManager: NewTextBackendManager("status"),
		logger:             logger,
	}
	r.SetTextRenderMode(TextRenderModeDirectWrite)
	return r
}

// BuildStatusText 根据状态和显示选项构建合并后的状态文本
func BuildStatusText(state StatusState, showMode, showPunct, showFullWidth bool) string {
	var parts []string
	if showMode && state.ModeLabel != "" {
		parts = append(parts, state.ModeLabel)
	}
	if showPunct && state.PunctLabel != "" {
		parts = append(parts, state.PunctLabel)
	}
	if showFullWidth && state.WidthLabel != "" {
		parts = append(parts, state.WidthLabel)
	}
	return strings.Join(parts, " ")
}

// Render 将状态信息渲染为 RGBA 图像
func (r *StatusRenderer) Render(state StatusState, cfg StatusWindowConfig) *image.RGBA {
	text := BuildStatusText(state, cfg.ShowMode, cfg.ShowPunct, cfg.ShowFullWidth)
	if text == "" {
		return nil
	}

	scale := GetDPIScale()
	fontSize := cfg.FontSize * scale
	padding := 6.0 * scale
	minWidth := 32.0 * scale
	borderRadius := cfg.BorderRadius * scale

	r.mu.Lock()
	td := r.TextDrawer()
	r.mu.Unlock()

	// 测量文本宽度
	tw := td.MeasureString(text, fontSize)
	width := tw + padding*2
	if width < minWidth {
		width = minWidth
	}
	height := fontSize + padding*2

	// 获取颜色
	bgColor, textColor := r.getColors(cfg)

	// 应用透明度
	bgColor = applyOpacity(bgColor, cfg.Opacity)

	// 绘制圆角矩形背景
	dc := gg.NewContext(int(width), int(height))
	dc.SetColor(bgColor)
	dc.DrawRoundedRectangle(0, 0, width, height, borderRadius)
	dc.Fill()

	// 绘制文本（居中）
	img := dc.Image().(*image.RGBA)
	textX := (width - tw) / 2
	textY := padding + fontSize*0.8
	td.BeginDraw(img)
	td.DrawString(text, textX, textY, fontSize, textColor)
	td.EndDraw()

	DrawDebugBanner(img)
	return img
}

// getColors 获取渲染颜色，优先级：自定义颜色 > 主题颜色 > 默认颜色
func (r *StatusRenderer) getColors(cfg StatusWindowConfig) (bgColor, textColor color.Color) {
	// 默认颜色
	var bg color.Color = color.RGBA{60, 60, 60, 240}
	var text color.Color = color.RGBA{255, 255, 255, 255}

	// 尝试主题颜色
	r.mu.Lock()
	resolved := r.resolvedTheme
	r.mu.Unlock()

	if resolved != nil {
		bg = resolved.ModeIndicator.BackgroundColor
		text = resolved.ModeIndicator.TextColor
	}

	// 自定义颜色优先级最高
	if cfg.BackgroundColor != "" {
		if c, ok := parseHexColor(cfg.BackgroundColor); ok {
			bg = c
		}
	}
	if cfg.TextColor != "" {
		if c, ok := parseHexColor(cfg.TextColor); ok {
			text = c
		}
	}

	return bg, text
}

// applyOpacity 将透明度应用到颜色的 alpha 通道
func applyOpacity(c color.Color, opacity float64) color.Color {
	if opacity <= 0 {
		opacity = 0
	}
	if opacity > 1 {
		opacity = 1
	}
	r, g, b, a := c.RGBA()
	newA := uint8(float64(a>>8) * opacity)
	return color.RGBA{uint8(r >> 8), uint8(g >> 8), uint8(b >> 8), newA}
}

// parseHexColor 解析 "#RRGGBB" 或 "#RRGGBBAA" 格式的颜色字符串
func parseHexColor(s string) (color.RGBA, bool) {
	if len(s) == 0 || s[0] != '#' {
		return color.RGBA{}, false
	}
	hex := s[1:]
	var r, g, b, a uint8
	switch len(hex) {
	case 6:
		if _, err := hexToBytes(hex, &r, &g, &b); err {
			return color.RGBA{}, false
		}
		a = 255
	case 8:
		if _, err := hexToBytes(hex[:6], &r, &g, &b); err {
			return color.RGBA{}, false
		}
		if _, err := hexToBytes(hex[6:8], &a); err {
			return color.RGBA{}, false
		}
	default:
		return color.RGBA{}, false
	}
	return color.RGBA{r, g, b, a}, true
}

// hexToBytes 将十六进制字符串解析为字节数组，每2个字符一个字节
func hexToBytes(hex string, out ...*uint8) (int, bool) {
	idx := 0
	for i := 0; i+1 < len(hex) && idx < len(out); i += 2 {
		hi, ok1 := hexDigit(hex[i])
		lo, ok2 := hexDigit(hex[i+1])
		if !ok1 || !ok2 {
			return idx, true
		}
		*out[idx] = hi<<4 | lo
		idx++
	}
	return idx, false
}

// hexDigit 将单个十六进制字符转为数值
func hexDigit(c byte) (uint8, bool) {
	switch {
	case c >= '0' && c <= '9':
		return c - '0', true
	case c >= 'a' && c <= 'f':
		return c - 'a' + 10, true
	case c >= 'A' && c <= 'F':
		return c - 'A' + 10, true
	default:
		return 0, false
	}
}

// SetTheme 设置主题
func (r *StatusRenderer) SetTheme(resolved *theme.ResolvedTheme) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.resolvedTheme = resolved
}

// Close 释放渲染资源
func (r *StatusRenderer) Close() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.TextBackendManager.Close()
}

// SetFontPath 设置字体路径
func (r *StatusRenderer) SetFontPath(path string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.TextBackendManager.SetFontPath(path)
}

// SetTextRenderMode 切换文本渲染模式
func (r *StatusRenderer) SetTextRenderMode(mode TextRenderMode) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.TextBackendManager.SetTextRenderMode(mode)
}

// SetGDIFontParams 更新 GDI 字体参数
func (r *StatusRenderer) SetGDIFontParams(weight int, scale float64) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.TextBackendManager.SetGDIFontParams(weight, scale)
}
