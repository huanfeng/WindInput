// Package theme provides theme configuration for WindInput UI
package theme

// 注意：主题风格常量（system/light/dark）唯一权威定义在 pkg/config（ThemeStyle 类型）。
// 本包不再重复声明，调用方请直接使用 config.ThemeStyleSystem / ThemeStyleLight / ThemeStyleDark。

// ThemeMeta contains theme metadata
type ThemeMeta struct {
	Name    string `yaml:"name" json:"name"`
	Version string `yaml:"version" json:"version"`
	Author  string `yaml:"author" json:"author"`
	Order   int    `yaml:"order" json:"order"` // Sort order, lower = first. Third-party themes get +100
}

// Theme 是 v3 主题的顶层结构（见 docs/design/theme-schema-v3.md「顶层结构」）。
//
// v3-C「base 单链继承」：
//   - 旧的 `layout: <id>` / `palette: <id>` 外链 + `overrides` 机制已删除；
//   - 改为顶层内联块 colors / views / behavior，配 `base` 单链继承复用别人的配置。
//   - 加载器把 base 链与 self 在**原始未求值 Theme** 上 deepMerge，合并后再统一 resolve
//     （先合并后求值——见 manager.go loadRawTheme / deepMergeTheme）。
//
// V3-D：原顶层 `layout` 块（LayoutSchema/density 基线）已删除——其它窗口几何随 P8 几何
// View 化后由 views 节点或 internal/ui 内置常量承载，不再有独立几何来源。
//
// 各块均为指针/可空，未写即缺省（base 提供、或引擎基线兜底）。
type Theme struct {
	Meta ThemeMeta `yaml:"meta" json:"meta"`

	// Base 单链继承的基主题 ID（按 themeDirs 找 <base>/theme.yaml，与普通主题同路径）。
	// 空=无继承。链上禁止成环（loadRawTheme 检测并报错）。
	Base string `yaml:"base,omitempty" json:"base,omitempty"`

	// 内联块（v3）：
	//   - Colors（yaml: colors）：扁平 LightDark 颜色 token 表（PaletteSchema）。
	//   - Views：盒模型 View 外观（所有窗口几何 + token 引用）。
	//   - Behavior：用户可覆盖的主题推荐默认。
	Colors    *PaletteSchema         `yaml:"colors,omitempty" json:"colors,omitempty"`
	Views     *Views                 `yaml:"views,omitempty" json:"views,omitempty"`
	Behavior  *Behavior              `yaml:"behavior,omitempty" json:"behavior,omitempty"`
	Resources map[string]ResourceRef `yaml:"resources,omitempty" json:"resources,omitempty"`
}

// HasV3Schema 返回 true 表示该 Theme 提供了 v3 颜色块（colors）。
// 无 colors 块的主题（如 emptyTheme 兜底）解析为 nil resolvedV25。
func (t *Theme) HasV3Schema() bool {
	return t.Colors != nil
}
