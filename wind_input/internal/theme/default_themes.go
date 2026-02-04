package theme

// DefaultTheme returns the built-in light theme
func DefaultTheme() *Theme {
	return &Theme{
		Meta: ThemeMeta{
			Name:    "默认主题",
			Version: "1.0",
			Author:  "WindInput",
		},
		CandidateWindow: CandidateWindowColors{
			BackgroundColor: "#FFFFFFF5", // Slightly transparent white
			BorderColor:     "#C8C8C8",
			TextColor:       "#1E1E1E",
			IndexColor:      "#FFFFFF",
			IndexBgColor:    "#4285F4", // Blue
			HoverBgColor:    "#E6F0FF", // Light blue
			InputBgColor:    "#F0F0F0",
			InputTextColor:  "#646464",
			CommentColor:    "#969696",
			ShadowColor:     "#0000000F", // 15/255 alpha
		},
		Toolbar: ToolbarColors{
			BackgroundColor:     "#FFFFFFFA",
			BorderColor:         "#C7D1E0", // Light blue-gray
			GripColor:           "#99ADC7B3",
			ModeChineseBgColor:  "#339AF5", // Bright blue
			ModeEnglishBgColor:  "#737F94", // Slate gray
			ModeTextColor:       "#FFFFFF",
			FullWidthOnBgColor:  "#2EB899", // Teal green
			FullWidthOffBgColor: "#E6EAEF", // Light cool gray
			FullWidthOnColor:    "#FFFFFF",
			FullWidthOffColor:   "#59667A",
			PunctChineseBgColor: "#F58543", // Coral orange
			PunctEnglishBgColor: "#E6EAEF",
			PunctChineseColor:   "#FFFFFF",
			PunctEnglishColor:   "#59667A",
			SettingsBgColor:     "#E6EAEF",
			SettingsIconColor:   "#7A66B8", // Muted purple
			SettingsHoleColor:   "#E6EAEF",
		},
		PopupMenu: PopupMenuColors{
			BackgroundColor: "#FFFFFF",
			BorderColor:     "#C7C7C7",
			TextColor:       "#000000",
			DisabledColor:   "#A1A1A1",
			HoverBgColor:    "#0078D4", // Windows blue
			HoverTextColor:  "#FFFFFF",
			SeparatorColor:  "#DBDBDB",
		},
		Tooltip: TooltipColors{
			BackgroundColor: "#3C3C3CF0",
			TextColor:       "#FFFFFF",
		},
		ModeIndicator: ModeIndicatorColors{
			BackgroundColor: "#323232E6",
			TextColor:       "#FFFFFF",
		},
	}
}

// DarkTheme returns the built-in dark theme
func DarkTheme() *Theme {
	return &Theme{
		Meta: ThemeMeta{
			Name:    "暗色主题",
			Version: "1.0",
			Author:  "WindInput",
		},
		CandidateWindow: CandidateWindowColors{
			BackgroundColor: "#2D2D2DF5",
			BorderColor:     "#404040",
			TextColor:       "#E0E0E0",
			IndexColor:      "#FFFFFF",
			IndexBgColor:    "#4285F4",
			HoverBgColor:    "#3D4A5C",
			InputBgColor:    "#3A3A3A",
			InputTextColor:  "#B0B0B0",
			CommentColor:    "#808080",
			ShadowColor:     "#0000001A",
		},
		Toolbar: ToolbarColors{
			BackgroundColor:     "#2D2D2DFA",
			BorderColor:         "#404040",
			GripColor:           "#5A5A5AB3",
			ModeChineseBgColor:  "#339AF5",
			ModeEnglishBgColor:  "#5A5A5A",
			ModeTextColor:       "#FFFFFF",
			FullWidthOnBgColor:  "#2EB899",
			FullWidthOffBgColor: "#404040",
			FullWidthOnColor:    "#FFFFFF",
			FullWidthOffColor:   "#B0B0B0",
			PunctChineseBgColor: "#F58543",
			PunctEnglishBgColor: "#404040",
			PunctChineseColor:   "#FFFFFF",
			PunctEnglishColor:   "#B0B0B0",
			SettingsBgColor:     "#404040",
			SettingsIconColor:   "#9B8CCE",
			SettingsHoleColor:   "#404040",
		},
		PopupMenu: PopupMenuColors{
			BackgroundColor: "#2D2D2D",
			BorderColor:     "#404040",
			TextColor:       "#E0E0E0",
			DisabledColor:   "#707070",
			HoverBgColor:    "#0078D4",
			HoverTextColor:  "#FFFFFF",
			SeparatorColor:  "#404040",
		},
		Tooltip: TooltipColors{
			BackgroundColor: "#1E1E1EF0",
			TextColor:       "#E0E0E0",
		},
		ModeIndicator: ModeIndicatorColors{
			BackgroundColor: "#1E1E1EE6",
			TextColor:       "#E0E0E0",
		},
	}
}

// GetBuiltinTheme returns a built-in theme by name
func GetBuiltinTheme(name string) *Theme {
	switch name {
	case "dark", "暗色主题":
		return DarkTheme()
	default:
		return DefaultTheme()
	}
}
