package theme

import "image/color"

// ResolvedV25 是 v2.5 schema 解析后的最终形态，所有引用已展开、所有缺省字段已派生填充。
// P5 起为渲染层唯一解析结果来源（adapter/ResolvedTheme/v2 已退役）。
type ResolvedV25 struct {
	Meta    ThemeMeta
	Layout  ResolvedLayout
	Palette ResolvedPalette
	Views   *Views // 盒模型 View 外观（v2.6 P2）；nil=主题未提供 views，渲染器用合成桥
}

// ResolvedLayout layout 的解析形态（与 LayoutSchema 同形，标量类型不变）
type ResolvedLayout struct {
	Density string
	Scale   float64

	CandidateWindow ResolvedCandidateWindowLayout
	Toolbar         ToolbarLayout
	Status          StatusLayout
	Tooltip         TooltipLayout
	PopupMenu       PopupMenuLayout
	Toast           ToastLayout
}

// ResolvedCandidateWindowLayout 解析后的候选窗布局
type ResolvedCandidateWindowLayout struct {
	WindowPadding Padding
	BandGap       int
	BorderWidth   int
	BorderRadius  int
	PreeditBar    BandLayout
	CandidateList CandidateListLayout
	FooterBar     BandLayout
}

// ResolvedPalette palette 的解析形态，所有颜色已 ParseColor 完成
type ResolvedPalette struct {
	IsDark  bool
	Primary color.Color

	// 顶层语义色
	Bg       color.Color
	Surface  color.Color
	Border   color.Color
	Text     color.Color
	TextDim  color.Color
	TextHint color.Color
	Accent   color.Color
	OnAccent color.Color
	Shadow   color.Color

	CandidateWindow ResolvedCandidateWindowPalette
	Toolbar         ResolvedToolbarPalette
	PopupMenu       ResolvedPopupMenuPalette
	Tooltip         ResolvedTooltipPalette
	Status          ResolvedStatusPalette
	Toast           ResolvedToastPalette

	Background *ResolvedBackground // nil 表示无背景图
}

type ResolvedCandidateWindowPalette struct {
	Background   color.Color
	Border       color.Color
	Text         color.Color
	Comment      color.Color
	IndexBg      color.Color
	IndexText    color.Color
	HoverBg      color.Color
	SelectedBg   color.Color
	SelectedText color.Color
	PreeditBg    color.Color
	PreeditText  color.Color
	AccentBar    color.Color
}

type ResolvedToolbarPalette struct {
	Background       color.Color
	Border           color.Color
	Grip             color.Color
	ModeChineseBg    color.Color
	ModeEnglishBg    color.Color
	ModeText         color.Color
	FullWidthOffBg   color.Color
	FullWidthOffText color.Color
	PunctEnglishBg   color.Color
	PunctEnglishText color.Color
	SettingsBg       color.Color
	SettingsIcon     color.Color
	SettingsHole     color.Color
}

type ResolvedPopupMenuPalette struct {
	Background color.Color
	Border     color.Color
	Text       color.Color
	Disabled   color.Color
	HoverBg    color.Color
	HoverText  color.Color
	Separator  color.Color
}

type ResolvedTooltipPalette struct {
	Background color.Color
	Text       color.Color
}

type ResolvedStatusPalette struct {
	Background color.Color
	Border     color.Color
	Text       color.Color
}

type ResolvedToastPalette struct {
	Background color.Color
	Text       color.Color
}

// ResolvedBackground 背景图解析后
type ResolvedBackground struct {
	ImagePath string // 绝对路径或 data: URI
	Mode      string // nine_slice | stretch | tile | center
	Slice     Padding
	Opacity   float64
}
