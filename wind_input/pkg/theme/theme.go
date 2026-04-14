// Package theme provides theme configuration for WindInput UI
package theme

import (
	"image/color"
)

// ThemeStyle constants for theme appearance mode
const (
	ThemeStyleSystem = "system" // Follow system dark/light mode
	ThemeStyleLight  = "light"  // Always light
	ThemeStyleDark   = "dark"   // Always dark
)

// ThemeMeta contains theme metadata
type ThemeMeta struct {
	Name    string `yaml:"name" json:"name"`
	Version string `yaml:"version" json:"version"`
	Author  string `yaml:"author" json:"author"`
	Order   int    `yaml:"order" json:"order"` // Sort order, lower = first. Third-party themes get +100
}

// CandidateWindowColors defines colors for the candidate window
type CandidateWindowColors struct {
	BackgroundColor string `yaml:"background_color" json:"background_color"`
	BorderColor     string `yaml:"border_color" json:"border_color"`
	TextColor       string `yaml:"text_color" json:"text_color"`
	IndexColor      string `yaml:"index_color" json:"index_color"`
	IndexBgColor    string `yaml:"index_bg_color" json:"index_bg_color"`
	HoverBgColor    string `yaml:"hover_bg_color" json:"hover_bg_color"`
	SelectedBgColor string `yaml:"selected_bg_color" json:"selected_bg_color"` // Background for keyboard-selected candidate
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

// CandidateWindowStyle defines rendering style options for the candidate window
type CandidateWindowStyle struct {
	IndexStyle         string  `yaml:"index_style" json:"index_style"`                   // "circle" (default) or "text"
	AccentBarColor     string  `yaml:"accent_bar_color" json:"accent_bar_color"`         // Left accent bar color, empty = no bar
	IndexFontWeight    int     `yaml:"index_font_weight" json:"index_font_weight"`       // Index number font weight (100-900), 0 = use global weight
	ItemPaddingLeft    float64 `yaml:"item_padding_left" json:"item_padding_left"`       // Left padding of each candidate item (px, 0 = default 8)
	ItemPaddingRight   float64 `yaml:"item_padding_right" json:"item_padding_right"`     // Right padding of each candidate item (px, 0 = default 8)
	WindowPaddingX     float64 `yaml:"window_padding_x" json:"window_padding_x"`         // Horizontal window padding (px, 0 = default 10)
	WindowPaddingY     float64 `yaml:"window_padding_y" json:"window_padding_y"`         // Vertical window padding (px, 0 = default 10)
	CornerRadius       float64 `yaml:"corner_radius" json:"corner_radius"`               // Window corner radius (px, 0 = default 8)
	RowHeight          float64 `yaml:"row_height" json:"row_height"`                     // Candidate row height (px, 0 = default 32)
	IndexMarginRight   float64 `yaml:"index_margin_right" json:"index_margin_right"`     // Gap between index and candidate text (px, 0 = default 4)
	TextMarginRight    float64 `yaml:"text_margin_right" json:"text_margin_right"`       // Gap after candidate text (px, 0 = default 4)
	CommentMarginLeft  float64 `yaml:"comment_margin_left" json:"comment_margin_left"`   // Gap between candidate text and comment (px, 0 = default 8)
	CommentMarginRight float64 `yaml:"comment_margin_right" json:"comment_margin_right"` // Gap after comment to item right edge (px, 0 = default 4)
	VerticalMinWidth   float64 `yaml:"vertical_min_width" json:"vertical_min_width"`     // Vertical layout minimum width (px, 0 = auto)
	VerticalMaxWidth   float64 `yaml:"vertical_max_width" json:"vertical_max_width"`     // Vertical layout maximum width (px, 0 = default 600)
	HorizontalMinWidth float64 `yaml:"horizontal_min_width" json:"horizontal_min_width"` // Horizontal layout minimum width (px, 0 = default 200)
	HorizontalMaxWidth float64 `yaml:"horizontal_max_width" json:"horizontal_max_width"` // Horizontal layout maximum width (px, 0 = no limit)
	AlwaysShowPager    bool    `yaml:"always_show_pager" json:"always_show_pager"`       // Always show page navigation (disable buttons when not navigable)
	ShowPageNumber     *bool   `yaml:"show_page_number" json:"show_page_number"`         // Show page number text (e.g. "1/3"), nil = true (default show)
}

// ThemeVariant contains all color sections for one appearance mode (light or dark)
type ThemeVariant struct {
	CandidateWindow CandidateWindowColors `yaml:"candidate_window" json:"candidate_window"`
	Toolbar         ToolbarColors         `yaml:"toolbar" json:"toolbar"`
	PopupMenu       PopupMenuColors       `yaml:"popup_menu" json:"popup_menu"`
	Tooltip         TooltipColors         `yaml:"tooltip" json:"tooltip"`
	ModeIndicator   ModeIndicatorColors   `yaml:"mode_indicator" json:"mode_indicator"`
}

// Theme represents a complete theme configuration.
// Supports two formats:
//   - New format: light/dark variants under "light:" and "dark:" keys
//   - Legacy format: colors at top level (treated as single variant for both modes)
type Theme struct {
	Meta  ThemeMeta            `yaml:"meta" json:"meta"`
	Style CandidateWindowStyle `yaml:"style" json:"style"`

	// New format: light/dark variants
	Light *ThemeVariant `yaml:"light,omitempty" json:"light,omitempty"`
	Dark  *ThemeVariant `yaml:"dark,omitempty" json:"dark,omitempty"`

	// Legacy format: top-level colors (backward compatible with old theme files)
	CandidateWindow CandidateWindowColors `yaml:"candidate_window" json:"candidate_window"`
	Toolbar         ToolbarColors         `yaml:"toolbar" json:"toolbar"`
	PopupMenu       PopupMenuColors       `yaml:"popup_menu" json:"popup_menu"`
	Tooltip         TooltipColors         `yaml:"tooltip" json:"tooltip"`
	ModeIndicator   ModeIndicatorColors   `yaml:"mode_indicator" json:"mode_indicator"`
}

// HasVariants returns true if the theme uses the new light/dark variant format
func (t *Theme) HasVariants() bool {
	return t.Light != nil || t.Dark != nil
}

// GetVariant returns the effective color variant for the given mode.
// For new format: returns the matching variant, falling back to the other if one is missing.
// For legacy format: returns a variant built from top-level colors.
func (t *Theme) GetVariant(isDark bool) *ThemeVariant {
	if t.HasVariants() {
		if isDark {
			if t.Dark != nil {
				return t.Dark
			}
			if t.Light != nil {
				return t.Light
			}
		} else {
			if t.Light != nil {
				return t.Light
			}
			if t.Dark != nil {
				return t.Dark
			}
		}
	}
	// Legacy format: use top-level colors
	return &ThemeVariant{
		CandidateWindow: t.CandidateWindow,
		Toolbar:         t.Toolbar,
		PopupMenu:       t.PopupMenu,
		Tooltip:         t.Tooltip,
		ModeIndicator:   t.ModeIndicator,
	}
}

// ResolvedCandidateWindowColors contains parsed colors for the candidate window
type ResolvedCandidateWindowColors struct {
	BackgroundColor color.Color
	BorderColor     color.Color
	TextColor       color.Color
	IndexColor      color.Color
	IndexBgColor    color.Color
	HoverBgColor    color.Color
	SelectedBgColor color.Color // Background for keyboard-selected candidate
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

// ResolvedCandidateWindowStyle contains parsed style options
type ResolvedCandidateWindowStyle struct {
	IndexStyle         string      // "circle" or "text"
	AccentBarColor     color.Color // nil if no accent bar
	HasAccentBar       bool
	IndexFontWeight    int     // Index number font weight (100-900), 0 = use global weight
	ItemPaddingLeft    float64 // Left padding of each candidate item (px, 0 = default 8)
	ItemPaddingRight   float64 // Right padding of each candidate item (px, 0 = default 8)
	WindowPaddingX     float64 // Horizontal window padding (px, 0 = default 10)
	WindowPaddingY     float64 // Vertical window padding (px, 0 = default 10)
	CornerRadius       float64 // Window corner radius (px, 0 = default 8)
	RowHeight          float64 // Candidate row height (px, 0 = default 32)
	IndexMarginRight   float64 // Gap between index and candidate text (px, 0 = default 4)
	TextMarginRight    float64 // Gap after candidate text (px, 0 = default 4)
	CommentMarginLeft  float64 // Gap between candidate text and comment (px, 0 = default 8)
	CommentMarginRight float64 // Gap after comment to item right edge (px, 0 = default 4)
	VerticalMinWidth   float64 // Vertical layout minimum width (px, 0 = auto)
	VerticalMaxWidth   float64 // Vertical layout maximum width (px, 0 = default 600)
	HorizontalMinWidth float64 // Horizontal layout minimum width (px, 0 = default 200)
	HorizontalMaxWidth float64 // Horizontal layout maximum width (px, 0 = no limit)
	AlwaysShowPager    bool    // Always show page navigation
	ShowPageNumber     bool    // Show page number text (e.g. "1/3")
}

// ResolvedTheme contains all resolved (parsed) colors
type ResolvedTheme struct {
	Meta            ThemeMeta
	CandidateWindow ResolvedCandidateWindowColors
	Style           ResolvedCandidateWindowStyle
	Toolbar         ResolvedToolbarColors
	PopupMenu       ResolvedPopupMenuColors
	Tooltip         ResolvedTooltipColors
	ModeIndicator   ResolvedModeIndicatorColors
}

// resolveStyle parses the style configuration
func (t *Theme) resolveStyle() ResolvedCandidateWindowStyle {
	// ShowPageNumber defaults to true when not explicitly set
	showPageNumber := true
	if t.Style.ShowPageNumber != nil {
		showPageNumber = *t.Style.ShowPageNumber
	}
	style := ResolvedCandidateWindowStyle{
		IndexStyle:         "circle", // default
		IndexFontWeight:    t.Style.IndexFontWeight,
		ItemPaddingLeft:    t.Style.ItemPaddingLeft,
		ItemPaddingRight:   t.Style.ItemPaddingRight,
		WindowPaddingX:     t.Style.WindowPaddingX,
		WindowPaddingY:     t.Style.WindowPaddingY,
		CornerRadius:       t.Style.CornerRadius,
		RowHeight:          t.Style.RowHeight,
		IndexMarginRight:   t.Style.IndexMarginRight,
		TextMarginRight:    t.Style.TextMarginRight,
		CommentMarginLeft:  t.Style.CommentMarginLeft,
		CommentMarginRight: t.Style.CommentMarginRight,
		VerticalMinWidth:   t.Style.VerticalMinWidth,
		VerticalMaxWidth:   t.Style.VerticalMaxWidth,
		HorizontalMinWidth: t.Style.HorizontalMinWidth,
		HorizontalMaxWidth: t.Style.HorizontalMaxWidth,
		AlwaysShowPager:    t.Style.AlwaysShowPager,
		ShowPageNumber:     showPageNumber,
	}
	if t.Style.IndexStyle == "text" {
		style.IndexStyle = "text"
	}
	if t.Style.AccentBarColor != "" {
		style.AccentBarColor = MustParseHexColor(t.Style.AccentBarColor, color.RGBA{0, 120, 212, 255})
		style.HasAccentBar = true
	}
	return style
}

// Resolve parses all color strings into color.Color values.
// isDark selects the dark variant when the theme supports light/dark modes.
func (t *Theme) Resolve(isDark bool) *ResolvedTheme {
	v := t.GetVariant(isDark)

	// Choose default fallback colors based on mode
	defaults := lightDefaults
	if isDark {
		defaults = darkDefaults
	}

	return &ResolvedTheme{
		Meta:  t.Meta,
		Style: t.resolveStyle(),
		CandidateWindow: ResolvedCandidateWindowColors{
			BackgroundColor: MustParseHexColor(v.CandidateWindow.BackgroundColor, defaults.candidateBg),
			BorderColor:     MustParseHexColor(v.CandidateWindow.BorderColor, defaults.candidateBorder),
			TextColor:       MustParseHexColor(v.CandidateWindow.TextColor, defaults.candidateText),
			IndexColor:      MustParseHexColor(v.CandidateWindow.IndexColor, defaults.indexColor),
			IndexBgColor:    MustParseHexColor(v.CandidateWindow.IndexBgColor, defaults.indexBg),
			HoverBgColor:    MustParseHexColor(v.CandidateWindow.HoverBgColor, defaults.hoverBg),
			SelectedBgColor: MustParseHexColor(v.CandidateWindow.SelectedBgColor, defaults.selectedBg),
			InputBgColor:    MustParseHexColor(v.CandidateWindow.InputBgColor, defaults.inputBg),
			InputTextColor:  MustParseHexColor(v.CandidateWindow.InputTextColor, defaults.inputText),
			CommentColor:    MustParseHexColor(v.CandidateWindow.CommentColor, defaults.commentColor),
			ShadowColor:     MustParseHexColor(v.CandidateWindow.ShadowColor, defaults.shadowColor),
		},
		Toolbar: ResolvedToolbarColors{
			BackgroundColor:     MustParseHexColor(v.Toolbar.BackgroundColor, defaults.toolbarBg),
			BorderColor:         MustParseHexColor(v.Toolbar.BorderColor, defaults.toolbarBorder),
			GripColor:           MustParseHexColor(v.Toolbar.GripColor, defaults.toolbarGrip),
			ModeChineseBgColor:  MustParseHexColor(v.Toolbar.ModeChineseBgColor, defaults.modeChineseBg),
			ModeEnglishBgColor:  MustParseHexColor(v.Toolbar.ModeEnglishBgColor, defaults.modeEnglishBg),
			ModeTextColor:       MustParseHexColor(v.Toolbar.ModeTextColor, defaults.modeText),
			FullWidthOnBgColor:  MustParseHexColor(v.Toolbar.FullWidthOnBgColor, defaults.fullWidthOnBg),
			FullWidthOffBgColor: MustParseHexColor(v.Toolbar.FullWidthOffBgColor, defaults.fullWidthOffBg),
			FullWidthOnColor:    MustParseHexColor(v.Toolbar.FullWidthOnColor, defaults.fullWidthOnColor),
			FullWidthOffColor:   MustParseHexColor(v.Toolbar.FullWidthOffColor, defaults.fullWidthOffColor),
			PunctChineseBgColor: MustParseHexColor(v.Toolbar.PunctChineseBgColor, defaults.punctChineseBg),
			PunctEnglishBgColor: MustParseHexColor(v.Toolbar.PunctEnglishBgColor, defaults.punctEnglishBg),
			PunctChineseColor:   MustParseHexColor(v.Toolbar.PunctChineseColor, defaults.punctChineseColor),
			PunctEnglishColor:   MustParseHexColor(v.Toolbar.PunctEnglishColor, defaults.punctEnglishColor),
			SettingsBgColor:     MustParseHexColor(v.Toolbar.SettingsBgColor, defaults.settingsBg),
			SettingsIconColor:   MustParseHexColor(v.Toolbar.SettingsIconColor, defaults.settingsIcon),
			SettingsHoleColor:   MustParseHexColor(v.Toolbar.SettingsHoleColor, defaults.settingsHole),
		},
		PopupMenu: ResolvedPopupMenuColors{
			BackgroundColor: MustParseHexColor(v.PopupMenu.BackgroundColor, defaults.menuBg),
			BorderColor:     MustParseHexColor(v.PopupMenu.BorderColor, defaults.menuBorder),
			TextColor:       MustParseHexColor(v.PopupMenu.TextColor, defaults.menuText),
			DisabledColor:   MustParseHexColor(v.PopupMenu.DisabledColor, defaults.menuDisabled),
			HoverBgColor:    MustParseHexColor(v.PopupMenu.HoverBgColor, defaults.menuHoverBg),
			HoverTextColor:  MustParseHexColor(v.PopupMenu.HoverTextColor, defaults.menuHoverText),
			SeparatorColor:  MustParseHexColor(v.PopupMenu.SeparatorColor, defaults.menuSeparator),
		},
		Tooltip: ResolvedTooltipColors{
			BackgroundColor: MustParseHexColor(v.Tooltip.BackgroundColor, defaults.tooltipBg),
			TextColor:       MustParseHexColor(v.Tooltip.TextColor, defaults.tooltipText),
		},
		ModeIndicator: ResolvedModeIndicatorColors{
			BackgroundColor: MustParseHexColor(v.ModeIndicator.BackgroundColor, defaults.indicatorBg),
			TextColor:       MustParseHexColor(v.ModeIndicator.TextColor, defaults.indicatorText),
		},
	}
}

// defaultColors holds fallback colors for a mode
type defaultColors struct {
	candidateBg, candidateBorder, candidateText       color.Color
	indexColor, indexBg, hoverBg, selectedBg          color.Color
	inputBg, inputText, commentColor, shadowColor     color.Color
	toolbarBg, toolbarBorder, toolbarGrip             color.Color
	modeChineseBg, modeEnglishBg, modeText            color.Color
	fullWidthOnBg, fullWidthOffBg, fullWidthOnColor   color.Color
	fullWidthOffColor                                 color.Color
	punctChineseBg, punctEnglishBg, punctChineseColor color.Color
	punctEnglishColor                                 color.Color
	settingsBg, settingsIcon, settingsHole            color.Color
	menuBg, menuBorder, menuText, menuDisabled        color.Color
	menuHoverBg, menuHoverText, menuSeparator         color.Color
	tooltipBg, tooltipText                            color.Color
	indicatorBg, indicatorText                        color.Color
}

var lightDefaults = defaultColors{
	candidateBg: color.RGBA{255, 255, 255, 255}, candidateBorder: color.RGBA{200, 200, 200, 255},
	candidateText: color.RGBA{30, 30, 30, 255},
	indexColor:    color.RGBA{255, 255, 255, 255}, indexBg: color.RGBA{66, 133, 244, 255},
	hoverBg: color.RGBA{230, 240, 255, 255}, selectedBg: color.RGBA{230, 240, 255, 255},
	inputBg: color.RGBA{240, 240, 240, 255}, inputText: color.RGBA{100, 100, 100, 255},
	commentColor: color.RGBA{150, 150, 150, 255}, shadowColor: color.RGBA{0, 0, 0, 15},
	toolbarBg: color.RGBA{255, 255, 255, 255}, toolbarBorder: color.RGBA{199, 209, 224, 255},
	toolbarGrip:   color.RGBA{153, 173, 199, 179},
	modeChineseBg: color.RGBA{51, 154, 245, 255}, modeEnglishBg: color.RGBA{115, 127, 148, 255},
	modeText:      color.RGBA{255, 255, 255, 255},
	fullWidthOnBg: color.RGBA{46, 184, 153, 255}, fullWidthOffBg: color.RGBA{230, 234, 239, 255},
	fullWidthOnColor: color.RGBA{255, 255, 255, 255}, fullWidthOffColor: color.RGBA{89, 102, 122, 255},
	punctChineseBg: color.RGBA{245, 133, 67, 255}, punctEnglishBg: color.RGBA{230, 234, 239, 255},
	punctChineseColor: color.RGBA{255, 255, 255, 255}, punctEnglishColor: color.RGBA{89, 102, 122, 255},
	settingsBg: color.RGBA{230, 234, 239, 255}, settingsIcon: color.RGBA{122, 102, 184, 255},
	settingsHole: color.RGBA{230, 234, 239, 255},
	menuBg:       color.RGBA{255, 255, 255, 255}, menuBorder: color.RGBA{199, 199, 199, 255},
	menuText: color.RGBA{0, 0, 0, 255}, menuDisabled: color.RGBA{161, 161, 161, 255},
	menuHoverBg: color.RGBA{0, 120, 212, 255}, menuHoverText: color.RGBA{255, 255, 255, 255},
	menuSeparator: color.RGBA{219, 219, 219, 255},
	tooltipBg:     color.RGBA{60, 60, 60, 240}, tooltipText: color.RGBA{255, 255, 255, 255},
	indicatorBg: color.RGBA{50, 50, 50, 230}, indicatorText: color.RGBA{255, 255, 255, 255},
}

var darkDefaults = defaultColors{
	candidateBg: color.RGBA{45, 45, 45, 255}, candidateBorder: color.RGBA{64, 64, 64, 255},
	candidateText: color.RGBA{224, 224, 224, 255},
	indexColor:    color.RGBA{255, 255, 255, 255}, indexBg: color.RGBA{66, 133, 244, 255},
	hoverBg: color.RGBA{61, 74, 92, 255}, selectedBg: color.RGBA{61, 74, 92, 255},
	inputBg: color.RGBA{58, 58, 58, 255}, inputText: color.RGBA{176, 176, 176, 255},
	commentColor: color.RGBA{128, 128, 128, 255}, shadowColor: color.RGBA{0, 0, 0, 26},
	toolbarBg: color.RGBA{45, 45, 45, 255}, toolbarBorder: color.RGBA{64, 64, 64, 255},
	toolbarGrip:   color.RGBA{90, 90, 90, 179},
	modeChineseBg: color.RGBA{51, 154, 245, 255}, modeEnglishBg: color.RGBA{90, 90, 90, 255},
	modeText:      color.RGBA{255, 255, 255, 255},
	fullWidthOnBg: color.RGBA{46, 184, 153, 255}, fullWidthOffBg: color.RGBA{64, 64, 64, 255},
	fullWidthOnColor: color.RGBA{255, 255, 255, 255}, fullWidthOffColor: color.RGBA{176, 176, 176, 255},
	punctChineseBg: color.RGBA{245, 133, 67, 255}, punctEnglishBg: color.RGBA{64, 64, 64, 255},
	punctChineseColor: color.RGBA{255, 255, 255, 255}, punctEnglishColor: color.RGBA{176, 176, 176, 255},
	settingsBg: color.RGBA{64, 64, 64, 255}, settingsIcon: color.RGBA{155, 140, 206, 255},
	settingsHole: color.RGBA{64, 64, 64, 255},
	menuBg:       color.RGBA{45, 45, 45, 255}, menuBorder: color.RGBA{64, 64, 64, 255},
	menuText: color.RGBA{224, 224, 224, 255}, menuDisabled: color.RGBA{112, 112, 112, 255},
	menuHoverBg: color.RGBA{0, 120, 212, 255}, menuHoverText: color.RGBA{255, 255, 255, 255},
	menuSeparator: color.RGBA{64, 64, 64, 255},
	tooltipBg:     color.RGBA{30, 30, 30, 240}, tooltipText: color.RGBA{224, 224, 224, 255},
	indicatorBg: color.RGBA{30, 30, 30, 230}, indicatorText: color.RGBA{224, 224, 224, 255},
}
