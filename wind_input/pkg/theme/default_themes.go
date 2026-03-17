package theme

// emptyTheme returns an empty theme with no colors set.
// When resolved, all colors will use the fallback defaults defined in Resolve().
func emptyTheme() *Theme {
	return &Theme{
		Meta: ThemeMeta{
			Name:    "默认主题",
			Version: "1.0",
			Author:  "清风输入法",
		},
	}
}
