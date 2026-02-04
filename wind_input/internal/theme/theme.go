// Package theme provides theme configuration for WindInput UI
package theme

import (
	"image/color"
)

// ThemeMeta contains theme metadata
type ThemeMeta struct {
	Name    string `yaml:"name" json:"name"`
	Version string `yaml:"version" json:"version"`
	Author  string `yaml:"author" json:"author"`
}

// CandidateWindowColors defines colors for the candidate window
type CandidateWindowColors struct {
	BackgroundColor string `yaml:"background_color" json:"background_color"`
	BorderColor     string `yaml:"border_color" json:"border_color"`
	TextColor       string `yaml:"text_color" json:"text_color"`
	IndexColor      string `yaml:"index_color" json:"index_color"`
	IndexBgColor    string `yaml:"index_bg_color" json:"index_bg_color"`
	HoverBgColor    string `yaml:"hover_bg_color" json:"hover_bg_color"`
	InputBgColor    string `yaml:"input_bg_color" json:"input_bg_color"`
	InputTextColor  string `yaml:"input_text_color" json:"input_text_color"`
	CommentColor    string `yaml:"comment_color" json:"comment_color"`
	ShadowColor     string `yaml:"shadow_color" json:"shadow_color"`
}

// ToolbarColors defines colors for the toolbar
type ToolbarColors struct {
	BackgroundColor     string `yaml:"background_color" json:"background_color"`
	BorderColor         string `yaml:"border_color" json:"border_color"`
	GripColor           string `yaml:"grip_color" json:"grip_color"`
	ModeChineseBgColor  string `yaml:"mode_chinese_bg_color" json:"mode_chinese_bg_color"`
	ModeEnglishBgColor  string `yaml:"mode_english_bg_color" json:"mode_english_bg_color"`
	ModeTextColor       string `yaml:"mode_text_color" json:"mode_text_color"`
	FullWidthOnBgColor  string `yaml:"full_width_on_bg_color" json:"full_width_on_bg_color"`
	FullWidthOffBgColor string `yaml:"full_width_off_bg_color" json:"full_width_off_bg_color"`
	FullWidthOnColor    string `yaml:"full_width_on_color" json:"full_width_on_color"`
	FullWidthOffColor   string `yaml:"full_width_off_color" json:"full_width_off_color"`
	PunctChineseBgColor string `yaml:"punct_chinese_bg_color" json:"punct_chinese_bg_color"`
	PunctEnglishBgColor string `yaml:"punct_english_bg_color" json:"punct_english_bg_color"`
	PunctChineseColor   string `yaml:"punct_chinese_color" json:"punct_chinese_color"`
	PunctEnglishColor   string `yaml:"punct_english_color" json:"punct_english_color"`
	SettingsBgColor     string `yaml:"settings_bg_color" json:"settings_bg_color"`
	SettingsIconColor   string `yaml:"settings_icon_color" json:"settings_icon_color"`
	SettingsHoleColor   string `yaml:"settings_hole_color" json:"settings_hole_color"`
}

// PopupMenuColors defines colors for popup menus
type PopupMenuColors struct {
	BackgroundColor string `yaml:"background_color" json:"background_color"`
	BorderColor     string `yaml:"border_color" json:"border_color"`
	TextColor       string `yaml:"text_color" json:"text_color"`
	DisabledColor   string `yaml:"disabled_color" json:"disabled_color"`
	HoverBgColor    string `yaml:"hover_bg_color" json:"hover_bg_color"`
	HoverTextColor  string `yaml:"hover_text_color" json:"hover_text_color"`
	SeparatorColor  string `yaml:"separator_color" json:"separator_color"`
}

// TooltipColors defines colors for tooltips
type TooltipColors struct {
	BackgroundColor string `yaml:"background_color" json:"background_color"`
	TextColor       string `yaml:"text_color" json:"text_color"`
}

// ModeIndicatorColors defines colors for the mode indicator
type ModeIndicatorColors struct {
	BackgroundColor string `yaml:"background_color" json:"background_color"`
	TextColor       string `yaml:"text_color" json:"text_color"`
}

// Theme represents a complete theme configuration
type Theme struct {
	Meta            ThemeMeta             `yaml:"meta" json:"meta"`
	CandidateWindow CandidateWindowColors `yaml:"candidate_window" json:"candidate_window"`
	Toolbar         ToolbarColors         `yaml:"toolbar" json:"toolbar"`
	PopupMenu       PopupMenuColors       `yaml:"popup_menu" json:"popup_menu"`
	Tooltip         TooltipColors         `yaml:"tooltip" json:"tooltip"`
	ModeIndicator   ModeIndicatorColors   `yaml:"mode_indicator" json:"mode_indicator"`
}

// ResolvedCandidateWindowColors contains parsed colors for the candidate window
type ResolvedCandidateWindowColors struct {
	BackgroundColor color.Color
	BorderColor     color.Color
	TextColor       color.Color
	IndexColor      color.Color
	IndexBgColor    color.Color
	HoverBgColor    color.Color
	InputBgColor    color.Color
	InputTextColor  color.Color
	CommentColor    color.Color
	ShadowColor     color.Color
}

// ResolvedToolbarColors contains parsed colors for the toolbar
type ResolvedToolbarColors struct {
	BackgroundColor     color.Color
	BorderColor         color.Color
	GripColor           color.Color
	ModeChineseBgColor  color.Color
	ModeEnglishBgColor  color.Color
	ModeTextColor       color.Color
	FullWidthOnBgColor  color.Color
	FullWidthOffBgColor color.Color
	FullWidthOnColor    color.Color
	FullWidthOffColor   color.Color
	PunctChineseBgColor color.Color
	PunctEnglishBgColor color.Color
	PunctChineseColor   color.Color
	PunctEnglishColor   color.Color
	SettingsBgColor     color.Color
	SettingsIconColor   color.Color
	SettingsHoleColor   color.Color
}

// ResolvedPopupMenuColors contains parsed colors for popup menus
type ResolvedPopupMenuColors struct {
	BackgroundColor color.Color
	BorderColor     color.Color
	TextColor       color.Color
	DisabledColor   color.Color
	HoverBgColor    color.Color
	HoverTextColor  color.Color
	SeparatorColor  color.Color
}

// ResolvedTooltipColors contains parsed colors for tooltips
type ResolvedTooltipColors struct {
	BackgroundColor color.Color
	TextColor       color.Color
}

// ResolvedModeIndicatorColors contains parsed colors for the mode indicator
type ResolvedModeIndicatorColors struct {
	BackgroundColor color.Color
	TextColor       color.Color
}

// ResolvedTheme contains all resolved (parsed) colors
type ResolvedTheme struct {
	Meta            ThemeMeta
	CandidateWindow ResolvedCandidateWindowColors
	Toolbar         ResolvedToolbarColors
	PopupMenu       ResolvedPopupMenuColors
	Tooltip         ResolvedTooltipColors
	ModeIndicator   ResolvedModeIndicatorColors
}

// Resolve parses all color strings into color.Color values
func (t *Theme) Resolve() *ResolvedTheme {
	return &ResolvedTheme{
		Meta: t.Meta,
		CandidateWindow: ResolvedCandidateWindowColors{
			BackgroundColor: MustParseHexColor(t.CandidateWindow.BackgroundColor, color.RGBA{255, 255, 255, 245}),
			BorderColor:     MustParseHexColor(t.CandidateWindow.BorderColor, color.RGBA{200, 200, 200, 255}),
			TextColor:       MustParseHexColor(t.CandidateWindow.TextColor, color.RGBA{30, 30, 30, 255}),
			IndexColor:      MustParseHexColor(t.CandidateWindow.IndexColor, color.RGBA{255, 255, 255, 255}),
			IndexBgColor:    MustParseHexColor(t.CandidateWindow.IndexBgColor, color.RGBA{66, 133, 244, 255}),
			HoverBgColor:    MustParseHexColor(t.CandidateWindow.HoverBgColor, color.RGBA{230, 240, 255, 255}),
			InputBgColor:    MustParseHexColor(t.CandidateWindow.InputBgColor, color.RGBA{240, 240, 240, 255}),
			InputTextColor:  MustParseHexColor(t.CandidateWindow.InputTextColor, color.RGBA{100, 100, 100, 255}),
			CommentColor:    MustParseHexColor(t.CandidateWindow.CommentColor, color.RGBA{150, 150, 150, 255}),
			ShadowColor:     MustParseHexColor(t.CandidateWindow.ShadowColor, color.RGBA{0, 0, 0, 15}),
		},
		Toolbar: ResolvedToolbarColors{
			BackgroundColor:     MustParseHexColor(t.Toolbar.BackgroundColor, color.RGBA{255, 255, 255, 250}),
			BorderColor:         MustParseHexColor(t.Toolbar.BorderColor, color.RGBA{199, 209, 224, 255}),
			GripColor:           MustParseHexColor(t.Toolbar.GripColor, color.RGBA{153, 173, 199, 179}),
			ModeChineseBgColor:  MustParseHexColor(t.Toolbar.ModeChineseBgColor, color.RGBA{51, 154, 245, 255}),
			ModeEnglishBgColor:  MustParseHexColor(t.Toolbar.ModeEnglishBgColor, color.RGBA{115, 127, 148, 255}),
			ModeTextColor:       MustParseHexColor(t.Toolbar.ModeTextColor, color.RGBA{255, 255, 255, 255}),
			FullWidthOnBgColor:  MustParseHexColor(t.Toolbar.FullWidthOnBgColor, color.RGBA{46, 184, 153, 255}),
			FullWidthOffBgColor: MustParseHexColor(t.Toolbar.FullWidthOffBgColor, color.RGBA{230, 234, 239, 255}),
			FullWidthOnColor:    MustParseHexColor(t.Toolbar.FullWidthOnColor, color.RGBA{255, 255, 255, 255}),
			FullWidthOffColor:   MustParseHexColor(t.Toolbar.FullWidthOffColor, color.RGBA{89, 102, 122, 255}),
			PunctChineseBgColor: MustParseHexColor(t.Toolbar.PunctChineseBgColor, color.RGBA{245, 133, 67, 255}),
			PunctEnglishBgColor: MustParseHexColor(t.Toolbar.PunctEnglishBgColor, color.RGBA{230, 234, 239, 255}),
			PunctChineseColor:   MustParseHexColor(t.Toolbar.PunctChineseColor, color.RGBA{255, 255, 255, 255}),
			PunctEnglishColor:   MustParseHexColor(t.Toolbar.PunctEnglishColor, color.RGBA{89, 102, 122, 255}),
			SettingsBgColor:     MustParseHexColor(t.Toolbar.SettingsBgColor, color.RGBA{230, 234, 239, 255}),
			SettingsIconColor:   MustParseHexColor(t.Toolbar.SettingsIconColor, color.RGBA{122, 102, 184, 255}),
			SettingsHoleColor:   MustParseHexColor(t.Toolbar.SettingsHoleColor, color.RGBA{230, 234, 239, 255}),
		},
		PopupMenu: ResolvedPopupMenuColors{
			BackgroundColor: MustParseHexColor(t.PopupMenu.BackgroundColor, color.RGBA{255, 255, 255, 255}),
			BorderColor:     MustParseHexColor(t.PopupMenu.BorderColor, color.RGBA{199, 199, 199, 255}),
			TextColor:       MustParseHexColor(t.PopupMenu.TextColor, color.RGBA{0, 0, 0, 255}),
			DisabledColor:   MustParseHexColor(t.PopupMenu.DisabledColor, color.RGBA{161, 161, 161, 255}),
			HoverBgColor:    MustParseHexColor(t.PopupMenu.HoverBgColor, color.RGBA{0, 120, 212, 255}),
			HoverTextColor:  MustParseHexColor(t.PopupMenu.HoverTextColor, color.RGBA{255, 255, 255, 255}),
			SeparatorColor:  MustParseHexColor(t.PopupMenu.SeparatorColor, color.RGBA{219, 219, 219, 255}),
		},
		Tooltip: ResolvedTooltipColors{
			BackgroundColor: MustParseHexColor(t.Tooltip.BackgroundColor, color.RGBA{60, 60, 60, 240}),
			TextColor:       MustParseHexColor(t.Tooltip.TextColor, color.RGBA{255, 255, 255, 255}),
		},
		ModeIndicator: ResolvedModeIndicatorColors{
			BackgroundColor: MustParseHexColor(t.ModeIndicator.BackgroundColor, color.RGBA{50, 50, 50, 230}),
			TextColor:       MustParseHexColor(t.ModeIndicator.TextColor, color.RGBA{255, 255, 255, 255}),
		},
	}
}
