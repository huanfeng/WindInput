package ui

// 从 RenderConfig + 候选数据构建候选窗的盒模型 View 树（v2.6 P1）。
//
// 本文件是"固定骨架 + 统一 View"思路的落地：旧渲染器里逐元素硬编码的 magic number，
// 在这里被翻译成各 View 的 margin/padding/border/fixed-size。引擎（viewbox.go/_paint.go）
// 只负责 measure/arrange/paint，对"候选窗"语义无感知。
//
// 当前覆盖横排核心：window / preedit_bar / candidate_list / item / index / text / comment
// 以及 selected/hover 背景、accent bar、pager、preedit 光标、ModeLabel、accent-glow。
// 暂未覆盖（后续迭代）：embedded preedit、竖排长候选省略号截断。

import (
	"image/color"

	"github.com/huanfeng/wind_input/pkg/config"
)

// buildEmbeddedPreedit 构建内嵌预编辑（PreeditEmbedded 模式）：编码 + ModeLabel 内嵌到候选行首，
// 与首个候选间留 16*scale 分隔；含内嵌光标。无内容返回 nil。
func (r *Renderer) buildEmbeddedPreedit(input string, cursorPos, rowH int, scale float64, sc func(float64) int) *View {
	cfg := &r.config
	if input == "" && cfg.ModeLabel == "" {
		return nil
	}
	children := make([]*View, 0, 2)
	if input != "" {
		children = append(children, &View{Text: input, TextStyle: TextStyle{FontSize: cfg.FontSize, Color: cfg.InputTextColor}})
	}
	if cfg.ModeLabel != "" {
		ml := &View{Text: cfg.ModeLabel, TextStyle: TextStyle{FontSize: cfg.IndexFontSize, Color: r.getCommentColor()}}
		if len(children) > 0 {
			ml.Margin = Edges{Left: sc(4 * scale)}
		}
		children = append(children, ml)
	}
	inline := &View{
		Layout: LayoutRow, CrossAlign: AlignCenter, FixedH: rowH,
		Margin:   Edges{Right: sc(16 * scale)},
		Children: children,
	}
	if input != "" && cursorPos >= 0 && cursorPos <= len(input) {
		cw := r.textDrawer.MeasureString(input[:cursorPos], cfg.FontSize)
		inline.Layers = append(inline.Layers, ImageLayer{
			Color: cfg.InputTextColor, Z: 1, Anchor: "left",
			OffsetX: int(cw + 0.5), W: maxInt(1, sc(1.5*scale)), H: int(float64(rowH) * 0.7),
		})
	}
	return inline
}

// buildPreeditBand 构建预编辑条（横竖排共用）：输入文本 + 光标 + 右对齐 ModeLabel + accent-glow 底色。
// inputH 为条高（横/竖排不同）。
func (r *Renderer) buildPreeditBand(input string, cursorPos, inputH int, scale float64, sc func(float64) int) *View {
	cfg := &r.config
	bgColor := cfg.InputBgColor
	if cfg.ModeAccentColor != nil {
		bgColor = blendColor(cfg.InputBgColor, cfg.ModeAccentColor, 35) // 临时拼音等模式：input 区半透 accent 叠加
	}
	children := []*View{{
		Text:      input,
		TextStyle: TextStyle{FontSize: cfg.FontSize, Color: cfg.InputTextColor},
	}}
	if cfg.ModeLabel != "" {
		children = append(children,
			&View{Grow: true}, // 弹性占位把标签推到右侧
			&View{Text: cfg.ModeLabel, TextStyle: TextStyle{FontSize: cfg.IndexFontSize, Color: r.getCommentColor()}},
		)
	}
	band := &View{
		Layout: LayoutRow, CrossAlign: AlignCenter, Stretch: true, FixedH: inputH,
		Padding:    Edges{Left: sc(8 * scale), Right: sc(8 * scale)},
		Background: Fill{Color: bgColor},
		Border:     Border{Radius: sc(4 * scale)},
		Children:   children,
	}
	if input != "" && cursorPos >= 0 && cursorPos <= len(input) {
		cw := r.textDrawer.MeasureString(input[:cursorPos], cfg.FontSize)
		band.Layers = append(band.Layers, ImageLayer{
			Color: cfg.InputTextColor, Z: 1, Anchor: "left",
			OffsetX: sc(8*scale) + int(cw+0.5), W: maxInt(1, sc(1.5*scale)), H: int(cfg.FontSize + 0.5),
		})
	}
	return band
}

// windowBorder 返回窗口边框：accent 模式(ModeAccentColor 非空)用更宽的 accent 色 glow 边框。
func (r *Renderer) windowBorder(radius int, sc func(float64) int, scale float64) Border {
	cfg := &r.config
	if cfg.ModeAccentColor != nil {
		return Border{Width: maxInt(1, sc(2.5*scale)), Color: cfg.ModeAccentColor, Radius: radius}
	}
	return Border{Width: 1, Color: cfg.BorderColor, Radius: radius}
}

// truncateToWidth 把 text 截断到不超过 avail 像素宽，超出时尾部加省略号。
func (r *Renderer) truncateToWidth(text string, fontSize, avail float64) string {
	if avail <= 0 || r.textDrawer.MeasureString(text, fontSize) <= avail {
		return text
	}
	const ell = "…"
	ellW := r.textDrawer.MeasureString(ell, fontSize)
	runes := []rune(text)
	for len(runes) > 0 {
		runes = runes[:len(runes)-1]
		if r.textDrawer.MeasureString(string(runes), fontSize)+ellW <= avail {
			return string(runes) + ell
		}
	}
	return ell
}

// blendColor 把 over 以 overAlpha/255 透明度叠加到 base 上，返回不透明结果。
func blendColor(base, over color.Color, overAlpha uint32) color.Color {
	br, bg, bb, _ := base.RGBA()
	or, og, ob, _ := over.RGBA()
	inv := 255 - overAlpha
	mix := func(b, o uint32) uint8 { return uint8(((o>>8)*overAlpha + (b>>8)*inv) / 255) }
	return color.RGBA{mix(br, or), mix(bg, og), mix(bb, ob), 255}
}

// buildHorizontalCandidateTree 构建横排候选窗 View 树。
// candWindowTree 是构建结果：窗口根 + 命中测试所需的关键 View。
type candWindowTree struct {
	root      *View
	items     []*View // 与 candidates 一一对应
	pagerUp   *View   // nil = 无翻页上键 / 不可用
	pagerDown *View
}

// (x,y) 由调用方在 Layout 时给定；本函数只描述结构与样式。
func (r *Renderer) buildHorizontalCandidateTree(
	candidates []Candidate,
	input string,
	cursorPos int,
	page, totalPages, selectedIndex, hoverIndex int,
	hoverPageBtn string,
) *candWindowTree {
	cfg := &r.config
	scale := GetDPIScale()
	sc := func(v float64) int { return int(v*scale + 0.5) }

	isTextIndex := cfg.IndexStyle == "text"
	isEmbedded := cfg.PreeditMode == config.PreeditEmbedded && !cfg.HidePreedit

	padX := pickF(cfg.WindowPaddingX, cfg.Padding)
	padY := pickF(cfg.WindowPaddingY, cfg.Padding)
	bgPadL := pickF(cfg.ItemPaddingLeft*scale, 8*scale)
	bgPadR := pickF(cfg.ItemPaddingRight*scale, 8*scale)
	indexMarginRight := pickF(cfg.IndexMarginRight*scale, 4*scale)
	commentMarginLeft := pickF(cfg.CommentMarginLeft*scale, 8*scale)

	itemSpacing := 12 * scale
	commentSize := cfg.IndexFontSize
	if isTextIndex {
		itemSpacing = 16 * scale
		commentSize = cfg.IndexFontSize + 2*scale
	}
	indexSize := maxF(18*scale, cfg.IndexFontSize+4*scale)
	rowH := int(cfg.ItemHeight + 0.5)
	commentColor := r.getCommentColor()

	// ---- 候选项 ----
	items := make([]*View, 0, len(candidates))
	for i, cand := range candidates {
		children := make([]*View, 0, 3)

		if cand.Index >= 0 {
			label := indexLabel(cfg.IndexLabels, cand.Index, cand.IndexLabel)
			if isTextIndex {
				children = append(children, &View{
					Text:      label,
					TextStyle: TextStyle{FontSize: cfg.IndexFontSize, Weight: cfg.IndexFontWeight, Color: cfg.IndexColor},
				})
			} else {
				d := int(indexSize + 0.5)
				children = append(children, &View{
					FixedW:     d,
					FixedH:     d,
					Background: Fill{Color: cfg.IndexBgColor},
					Border:     Border{Radius: d / 2},
					Layout:     LayoutStack,
					Children: []*View{{
						FixedW:    d,
						FixedH:    d,
						Text:      label,
						TextStyle: TextStyle{FontSize: cfg.IndexFontSize, Weight: cfg.IndexFontWeight, Color: cfg.IndexColor, Align: AlignCenter},
					}},
				})
			}
		}

		// 候选文字
		textChild := &View{
			Text:      candidateDisplayText(cand, cfg.CmdbarPrefix),
			TextStyle: TextStyle{FontSize: cfg.FontSize, Color: cfg.TextColor},
		}
		if len(children) > 0 {
			textChild.Margin = Edges{Left: sc(indexMarginRight)}
		}
		children = append(children, textChild)

		// 注释
		if cand.Comment != "" {
			children = append(children, &View{
				Text:      cand.Comment,
				TextStyle: TextStyle{FontSize: commentSize, Color: commentColor},
				Margin:    Edges{Left: sc(commentMarginLeft)},
			})
		}

		item := &View{
			Layout:     LayoutRow,
			CrossAlign: AlignCenter,
			Padding:    Edges{Left: sc(bgPadL), Right: sc(bgPadR)},
			FixedH:     rowH,
			Border:     Border{Radius: sc(4 * scale)},
			Children:   children,
		}
		if i == selectedIndex {
			item.Background = Fill{Color: cfg.SelectedBgColor}
			// accent 强调条：选中项左缘竖条（z<0 纯色层，垂直居中，高约行高 60%）
			if cfg.HasAccentBar && cfg.AccentBarColor != nil {
				barW := sc(3 * scale)
				item.Layers = []ImageLayer{{
					Color:   cfg.AccentBarColor,
					Z:       -1,
					Anchor:  "left",
					OffsetX: sc(1 * scale),
					W:       barW,
					H:       int(cfg.ItemHeight*0.6 + 0.5),
					Radius:  barW / 2,
				}}
			}
		} else if i == hoverIndex {
			item.Background = Fill{Color: cfg.HoverBgColor}
		}
		items = append(items, item)
	}

	// ---- 候选列表行：[内嵌预编辑?] + 候选项 + [翻页区?] ----
	pagerChildren, pagerUp, pagerDown := r.buildPager(scale, sc, isTextIndex, page, totalPages, hoverPageBtn, rowH)
	listChildren := make([]*View, 0, len(items)+4)
	if isEmbedded {
		if inline := r.buildEmbeddedPreedit(input, cursorPos, rowH, scale, sc); inline != nil {
			listChildren = append(listChildren, inline)
		}
	}
	listChildren = append(listChildren, items...)
	if len(pagerChildren) > 0 {
		pagerChildren[0].Margin = Edges{Left: sc(8 * scale)} // 与候选列表的分隔
		listChildren = append(listChildren, pagerChildren...)
	}

	// 候选框间隙：旧渲染器 effectiveSpacing=max(padL+padR, itemSpacing)，扣掉左右内边距后
	// 即相邻框之间的真实间隙（通常为 0，框相邻）。
	boxGap := maxF(itemSpacing-bgPadL-bgPadR, 0)
	list := &View{
		Layout:     LayoutRow,
		CrossAlign: AlignCenter, // 页码文本/箭头按钮在行内垂直居中
		Gap:        sc(boxGap),
		Children:   listChildren,
	}

	// ---- band 列表（preedit + 候选列表）----
	bands := make([]*View, 0, 2)
	if (input != "" || cfg.ModeLabel != "") && !cfg.HidePreedit && !isEmbedded {
		inputH := int(maxF(24*scale, cfg.FontSize*1.3) + 0.5)
		bands = append(bands, r.buildPreeditBand(input, cursorPos, inputH, scale, sc))
	}
	bands = append(bands, list)

	window := &View{
		Layout:     LayoutColumn,
		Gap:        sc(4 * scale),
		Padding:    Edges{Top: sc(padY), Right: sc(padX), Bottom: sc(padY), Left: sc(padX)},
		Background: Fill{Color: cfg.BackgroundColor, Image: cfg.BackgroundImage, Mode: cfg.BackgroundMode, Slice: cfg.BackgroundSlice, Opacity: cfg.BackgroundOpacity},
		Border:     r.windowBorder(int(cfg.CornerRadius+0.5), sc, scale),
		Shadow:     &ViewShadow{OffsetX: sc(2 * scale), OffsetY: sc(2 * scale), Color: r.getShadowColor()},
		Children:   bands,
	}
	return &candWindowTree{root: window, items: items, pagerUp: pagerUp, pagerDown: pagerDown}
}

func pickF(primary, fallback float64) float64 {
	if primary > 0 {
		return primary
	}
	return fallback
}

func maxF(a, b float64) float64 {
	if a > b {
		return a
	}
	return b
}
