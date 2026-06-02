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

// Theme represents a complete theme configuration（v2.5 schema：layout / palette / views）。
//
// v2/legacy 格式（light/dark variant、顶层颜色、style、Theme.Resolve、ResolvedTheme）已于 P5
// 退役——只支持 v2.5。渲染层统一消费 ResolvedV25（manager.ResolveV25 产出）。
type Theme struct {
	Meta ThemeMeta `yaml:"meta" json:"meta"`

	// v2.5 format: layout / palette 字段。值可为：
	//   - string: 共享零件 ID（外链形态），加载器到 themes/_layouts/ 或 _palettes/ 解析
	//   - map[string]any: 内联对象（内联形态），通过 yaml round-trip 解为 LayoutSchema/PaletteSchema
	Layout    any        `yaml:"layout,omitempty" json:"layout,omitempty"`
	Palette   any        `yaml:"palette,omitempty" json:"palette,omitempty"`
	Views     *Views     `yaml:"views,omitempty" json:"views,omitempty"` // 盒模型 View 外观（v2.6 P2）
	Overrides *Overrides `yaml:"overrides,omitempty" json:"overrides,omitempty"`
}

// Overrides 用于外链形态对引用的 layout/palette 做就地微调。
// 字段为 map[string]any，按 yaml 路径深度合并到被引用文件之上。
// 内联形态不使用此字段（直接在内联块里改即可）。
type Overrides struct {
	Layout  map[string]any `yaml:"layout,omitempty" json:"layout,omitempty"`
	Palette map[string]any `yaml:"palette,omitempty" json:"palette,omitempty"`
}

// HasV25Schema 返回 true 表示该 Theme 使用了 v2.5 的 layout/palette 字段。
// 非 v2.5 主题（无 layout/palette）已不被支持，加载时 resolvedV25 为 nil。
func (t *Theme) HasV25Schema() bool {
	return t.Layout != nil || t.Palette != nil
}
