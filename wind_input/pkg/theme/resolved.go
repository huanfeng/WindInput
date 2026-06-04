package theme

import "image/color"

// ResolvedV25 是 v2.5 schema 解析后的最终形态，所有引用已展开、所有缺省字段已派生填充。
// P5 起为渲染层唯一解析结果来源（adapter/ResolvedTheme/v2 已退役）。
type ResolvedV25 struct {
	Meta     ThemeMeta
	Palette  ResolvedPalette
	Views    *Views           // 盒模型 View 外观（v2.6 P2）；nil=主题未提供 views，渲染器用合成桥
	Behavior ResolvedBehavior // 行为配置（v2.6 P6）：defaultBehavior ⊕ 主题 behavior（用户 override 在 ui/config 层）

	// Resources 图片资源注册表（v2.6 P7-C）：名→绝对路径 / data: URI（相对路径已按 theme 目录解析）。
	// 渲染器据此把 ViewImage.ref 解码为位图（一次性缓存，非每帧）。
	Resources map[string]string
}

// ResolvedPalette palette 的解析形态，所有颜色已逐 token 在 isDark 环境下递归求值 + ParseColor 完成。
//
// v3（颜色系统 v3 化）：删 5 个嵌套窗口色组（candidate/menu/tooltip/status/toast），
// 全部颜色扁平进 Tokens；保留顶层语义便捷字段（从 Tokens 镜像填充，供 internal/ui 既有读法）；
// 保留 ResolvedToolbarPalette（其 13 字段从 Tokens["toolbar_*"] 填充，使 viewbox_toolbar 消费最小改动）。
type ResolvedPalette struct {
	IsDark  bool
	Primary color.Color

	// 顶层语义便捷字段（从 Tokens 镜像；供 internal/ui 既有字段读法）
	Bg       color.Color
	Surface  color.Color
	Border   color.Color
	Text     color.Color
	TextDim  color.Color
	TextHint color.Color
	Accent   color.Color
	OnAccent color.Color
	Shadow   color.Color

	// Tokens 全部解析后颜色 token（顶层语义 + selection/hover + 功能前缀 token）。
	// candidate_views / other_views 的 token resolver 统一查此表。
	Tokens map[string]color.Color

	// Toolbar 保留（颜色来源仍是 toolbar_* token），viewbox_toolbar 经此消费。
	Toolbar ResolvedToolbarPalette
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
