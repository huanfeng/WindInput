package ui

// viewbox_views_bridge.go — 临时合成桥（P2 切片-0）：把 RenderConfig 的外观字段
// 合成为 theme.ResolvedViews，供尚未迁移到 YAML views 的主题使用。
// 所有几何值统一为「逻辑像素，single-scale」（build 端乘一次 DPI scale）——
// 这顺带统一了旧 build 里 item padding/margin 的 double-scale 与 window padding 的
// single-scale 不一致（scale=1 行为不变，scale≠1 修正为正确）。
// 待所有种子主题迁移到 YAML views 后，本文件与 adapter.go 一并删除。

import (
	"image/color"
	"strings"

	"github.com/huanfeng/wind_input/pkg/theme"
)

// roundI 逻辑像素取整。
func roundI(v float64) int { return int(v + 0.5) }

// buildResolvedViews 从 RenderConfig + 运行时颜色方法合成 ResolvedViews（几何=逻辑像素 single-scale，
// 颜色=color.Color）。comment/shadow 颜色取自 r.getCommentColor()/getShadowColor()（无对应 cfg 字段）。
func (r *Renderer) buildResolvedViews() theme.ResolvedViews {
	cfg := r.config
	isTextIndex := cfg.IndexStyle == "text"

	winPadX := pickF(cfg.WindowPaddingX, cfg.Padding)
	winPadY := pickF(cfg.WindowPaddingY, cfg.Padding)

	spacing := 12
	if isTextIndex {
		spacing = 16
	}

	return theme.ResolvedViews{
		Window: theme.RVNode{
			PadTop:       roundI(winPadY),
			PadBottom:    roundI(winPadY),
			PadLeft:      roundI(winPadX),
			PadRight:     roundI(winPadX),
			BorderRadius: roundI(cfg.CornerRadius),
			BgColor:      cfg.BackgroundColor,
			BorderColor:  cfg.BorderColor,
		},
		PreeditBar: theme.RVNode{
			PadLeft:      8,
			PadRight:     8,
			BorderRadius: 4,
			FontSize:     cfg.FontSize,
			BgColor:      cfg.InputBgColor,
			TextColor:    cfg.InputTextColor,
		},
		Item: theme.RVNode{
			PadLeft:      roundI(pickF(cfg.ItemPaddingLeft, 8)),
			PadRight:     roundI(pickF(cfg.ItemPaddingRight, 8)),
			BorderRadius: roundI(pickF(cfg.ItemRadius, 4)),
			SelectedBg:   cfg.SelectedBgColor,
			HoverBg:      cfg.HoverBgColor,
		},
		Index: theme.RVNode{
			FontSize:   cfg.IndexFontSize,
			FontWeight: cfg.IndexFontWeight,
			BgColor:    cfg.IndexBgColor,
			TextColor:  cfg.IndexColor,
		},
		Text: theme.RVNode{
			MarginLeft: roundI(pickF(cfg.IndexMarginRight, 4)),
			FontSize:   cfg.FontSize,
			TextColor:  cfg.TextColor,
		},
		Comment: theme.RVNode{
			MarginLeft: roundI(pickF(cfg.CommentMarginLeft, 8)),
			TextColor:  r.getCommentColor(),
		},
		AccentBar: theme.RVNode{
			BgColor: cfg.AccentBarColor,
		},

		WindowGap:        4,
		ShadowOffset:     2,
		ItemHeight:       cfg.ItemHeight,
		ItemSpacing:      spacing,
		AccentBarWidth:   3,
		AccentBarOffset:  1,
		AccentBarHRatio:  0.6,
		VerticalMaxWidth: pickF(cfg.VerticalMaxWidth, 600),
		ShadowColor:      r.getShadowColor(),
	}
}

// resolveViewColor 解析 views 颜色字段：hex（#RRGGBB[AA]）或 ${name} token 映射候选窗语义色。
// 空 / 未知 token / 解析失败返回 nil（调用方据此不覆盖 base，保零回归）。
func resolveViewColor(s string, cand theme.ResolvedCandidateWindowPalette, accent color.Color) color.Color {
	if s == "" {
		return nil
	}
	if strings.HasPrefix(s, "${") && strings.HasSuffix(s, "}") {
		switch s[2 : len(s)-1] {
		case "background":
			return cand.Background
		case "border":
			return cand.Border
		case "text":
			return cand.Text
		case "index_bg":
			return cand.IndexBg
		case "index_text":
			return cand.IndexText
		case "hover_bg":
			return cand.HoverBg
		case "selected_bg":
			return cand.SelectedBg
		case "preedit_bg":
			return cand.PreeditBg
		case "preedit_text":
			return cand.PreeditText
		case "comment":
			return cand.Comment
		case "accent":
			return accent // CandidateWindow 无 accent，单独从 Style.AccentBarColor 传入
		case "shadow":
			// Shadow 在 ResolvedPalette 顶层而非 CandidateWindow；此处无源，返回 nil（不覆盖 base）。
			return nil
		}
		return nil // 未知 token
	}
	if c, err := theme.ParseHexColor(s); err == nil {
		return c
	}
	return nil
}

// refreshResolvedViews 重建 r.resolvedViews：合成桥 base ⊕ 主题 views 覆盖（几何+颜色）。
// 由渲染入口 render*V2 每次调用，确保直接调 render*V2 的路径（含测试）都已填充。
func (r *Renderer) refreshResolvedViews() {
	r.resolvedViews = r.buildResolvedViews()
	if r.themeViews != nil {
		var cand theme.ResolvedCandidateWindowPalette
		if r.resolvedV25 != nil {
			cand = r.resolvedV25.Palette.CandidateWindow
		}
		applyThemeViews(&r.resolvedViews, r.themeViews, cand, r.config.AccentBarColor)
	}
}

// applyThemeViews 把主题 YAML 的 views（仅显式字段）覆盖到合成桥 base：几何（padding/margin/
// border 尺寸）+ 颜色（token/hex 解析）。字号不覆盖——用户全局字号优先。
func applyThemeViews(rv *theme.ResolvedViews, tv *theme.Views, cand theme.ResolvedCandidateWindowPalette, accent color.Color) {
	apply := func(dst *theme.RVNode, src theme.ViewNode) {
		if src.Padding.Top != nil {
			dst.PadTop = *src.Padding.Top
		}
		if src.Padding.Right != nil {
			dst.PadRight = *src.Padding.Right
		}
		if src.Padding.Bottom != nil {
			dst.PadBottom = *src.Padding.Bottom
		}
		if src.Padding.Left != nil {
			dst.PadLeft = *src.Padding.Left
		}
		if src.Margin.Top != nil {
			dst.MarginTop = *src.Margin.Top
		}
		if src.Margin.Right != nil {
			dst.MarginRight = *src.Margin.Right
		}
		if src.Margin.Bottom != nil {
			dst.MarginBottom = *src.Margin.Bottom
		}
		if src.Margin.Left != nil {
			dst.MarginLeft = *src.Margin.Left
		}
		if src.Border.Radius != nil {
			dst.BorderRadius = *src.Border.Radius
		}
		if src.Border.Width != nil {
			dst.BorderWidth = *src.Border.Width
		}
		// 颜色（字号不覆盖：用户全局优先）
		if c := resolveViewColor(src.Background.Color, cand, accent); c != nil {
			dst.BgColor = c
		}
		if c := resolveViewColor(src.Border.Color, cand, accent); c != nil {
			dst.BorderColor = c
		}
		if c := resolveViewColor(src.Color, cand, accent); c != nil {
			dst.TextColor = c
		}
	}
	apply(&rv.Window, tv.Window)
	apply(&rv.PreeditBar, tv.PreeditBar)
	apply(&rv.CandidateList, tv.CandidateList)
	apply(&rv.Item, tv.Item)
	apply(&rv.Index, tv.Index)
	apply(&rv.Text, tv.Text)
	apply(&rv.Comment, tv.Comment)
	apply(&rv.AccentBar, tv.AccentBar)
	apply(&rv.FooterBar, tv.FooterBar)

	// item 的 selected/hover 背景（来自 states patch）
	if tv.Item.Selected != nil {
		if c := resolveViewColor(tv.Item.Selected.Background.Color, cand, accent); c != nil {
			rv.Item.SelectedBg = c
		}
	}
	if tv.Item.Hover != nil {
		if c := resolveViewColor(tv.Item.Hover.Background.Color, cand, accent); c != nil {
			rv.Item.HoverBg = c
		}
	}
}
