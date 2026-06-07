package theme

import "maps"

// merge_theme.go — v3-C「base 单链继承」的**原始 Theme 深合并**（先合并后求值）。
//
// 求值铁律（docs/design/theme-schema-v3.md「已定决策 5」）：继承作用于**未求值的 raw Theme**，
// 合并完成后才统一 resolve（derive→auto_dark→token 递归求值→ParseColor）。这样派生主题
// 只覆盖 colors.primary 时，全套派生语义色会基于**新 primary** 重新派生（而非沿用 base 旧派生值）。
//
// 合并规则：self（override）非空则覆盖 base；嵌套结构递归/逐字段；map 逐 key 覆盖。

// deepMergeTheme 把 override（通常是子主题/self）合并到 base（通常是父 base 主题）之上，
// 返回新的合并 Theme（不修改入参）。各块按其自然语义合并：
//   - Colors：base.Tokens ⊕ self.Tokens（self 同 key 覆盖）；primary/derive/auto_dark 同 key 覆盖。
//   - Views：复用 mergeViews（逐具名 View / mergeViewNode 递归）。
//   - Behavior：逐指针字段覆盖（self 非 nil 覆盖 base）。
//   - Resources：base ⊕ self（self 同 key 覆盖）。
//   - Meta/Base：由调用方在链合并完成后统一回填（此处取 override 的，链尾即 self）。
//
// V3-D：原 Layout 块已删（geometry View 化后无独立几何来源），不再有 layout 合并分支。
func deepMergeTheme(base, override *Theme) *Theme {
	out := &Theme{
		Meta: override.Meta,
		Base: override.Base,
	}
	out.Colors = mergePaletteSchema(base.Colors, override.Colors)
	out.Views = mergeViewsPtr(base.Views, override.Views)
	out.Behavior = mergeBehaviorRaw(base.Behavior, override.Behavior)
	out.Resources = mergeResourceMap(base.Resources, override.Resources)
	return out
}

// mergePaletteSchema 合并两个 colors 块（raw，未求值）。base 为 nil 取 override，反之亦然。
func mergePaletteSchema(base, override *PaletteSchema) *PaletteSchema {
	if override == nil {
		return base
	}
	if base == nil {
		return override
	}
	out := &PaletteSchema{
		Meta:     base.Meta,
		Primary:  base.Primary,
		Derive:   base.Derive,
		AutoDark: base.AutoDark,
		Tokens:   make(map[string]Color, len(base.Tokens)+len(override.Tokens)),
	}
	maps.Copy(out.Tokens, base.Tokens)
	maps.Copy(out.Tokens, override.Tokens) // self 同 key 覆盖
	if override.Meta.Name != "" {
		out.Meta = override.Meta
	}
	if !override.Primary.IsZero() {
		out.Primary = override.Primary
	}
	// derive：self 显式给出（enabled 或 algorithm 非零）才覆盖；否则沿用 base。
	if override.Derive.Enabled || override.Derive.Algorithm != "" {
		out.Derive = override.Derive
	}
	if override.AutoDark {
		out.AutoDark = override.AutoDark
	}
	return out
}

// mergeViewsPtr 合并两个 views 块（raw）。复用 mergeViews（base ⊕ override 逐具名 View 递归）。
func mergeViewsPtr(base, override *Views) *Views {
	if override == nil {
		return base
	}
	if base == nil {
		return override
	}
	merged := mergeViews(*base, *override)
	return &merged
}

// mergeBehaviorRaw 合并两个 behavior 块（raw）。逐指针字段覆盖：self 非 nil 覆盖 base。
func mergeBehaviorRaw(base, override *Behavior) *Behavior {
	if override == nil {
		return base
	}
	if base == nil {
		return override
	}
	out := *base
	if override.FontSize != nil {
		out.FontSize = override.FontSize
	}
	if override.AlwaysShowPager != nil {
		out.AlwaysShowPager = override.AlwaysShowPager
	}
	if override.ShowPageNumber != nil {
		out.ShowPageNumber = override.ShowPageNumber
	}
	if override.HidePager != nil {
		out.HidePager = override.HidePager
	}
	if override.VerticalMaxWidth != nil {
		out.VerticalMaxWidth = override.VerticalMaxWidth
	}
	return &out
}

// mergeResourceMap 合并 resources（base ⊕ override，self 同 key 覆盖）。
func mergeResourceMap(base, override map[string]ResourceRef) map[string]ResourceRef {
	if len(override) == 0 {
		return base
	}
	if len(base) == 0 {
		return override
	}
	out := make(map[string]ResourceRef, len(base)+len(override))
	maps.Copy(out, base)
	maps.Copy(out, override)
	return out
}
