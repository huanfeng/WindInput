package theme

import (
	"fmt"
	"image/color"
	"maps"
	"path/filepath"
	"strings"
)

// ResolveV25 把（已 base 合并的）v3 Theme 解析为 ResolvedV25。
// 调用方应先用 HasV3Schema() 判定。themeFileDir 用于解析背景图相对路径（self theme.yaml 所在目录）。
//
// v3-C：t.Colors 已是 base 链合并后的内联块（loadThemeFileWithDir 做 deepMergeTheme），
// 此处只负责**求值**（derive / token 递归求值 / 变体选取），不再做外链加载或 overrides。
// V3-D：layout/density 几何机制已删，其它窗口几何由 views 节点或 internal/ui 内置常量承载。
func (m *Manager) ResolveV25(t *Theme, isDark bool, themeFileDir string) (*ResolvedV25, error) {
	// 1. colors 块（已是合并后的内联 schema）。
	palette := t.Colors
	if palette == nil {
		return nil, fmt.Errorf("colors 块缺失（v3 主题须含 colors）")
	}

	// 2. palette 派生并展开引用
	fullPalette, err := finalizePalette(palette, isDark)
	if err != nil {
		return nil, fmt.Errorf("palette 派生失败: %w", err)
	}

	rv := &ResolvedV25{
		Meta:    t.Meta,
		Palette: fullPalette,
	}
	// 5. views（v2.6 P6 阶段2c）：defaultViews 基线 ⊕ 主题 views（mergeViews 逐字段覆盖），
	// rv.Views 始终非 nil。候选窗渲染器直接消费此结果（ResolveCandidateViews），合成桥退役中。
	// 独立窗口（Status/Tooltip/Toolbar/Menu）按各自字段指针判空，基线不含这些字段，故无 views
	// 块的主题仍回退到 palette 默认（零回归）。
	base := defaultViews()
	if t.Views != nil {
		merged := mergeViews(base, *t.Views)
		rv.Views = &merged
		if m.logger != nil {
			m.logger.Info("主题提供盒模型 views，候选窗外观经 YAML 驱动 (P6)", "theme", t.Meta.Name)
		}
	} else {
		rv.Views = &base
	}
	// 6. behavior（v2.6 P6）：defaultBehavior 基线 ⊕ 主题 behavior（非 nil 字段覆盖）。
	// 用户 override 不在此处——它在 ui/config 层注入（nil=跟随主题）。
	rv.Behavior = mergeBehavior(defaultBehavior(), t.Behavior)

	// 6.5 加载期校验（V3-D，已定决策 11）：views 各节点 ${token} 引用可达、image.ref 存在、
	// 颜色字面合法。失败 fail fast——不进入渲染（透明/黑屏难排查的根因前移到加载期）。
	if err := validateViews(rv.Views, fullPalette.Tokens, t.Resources); err != nil {
		return nil, fmt.Errorf("主题 %q 加载期校验失败: %w", t.Meta.Name, err)
	}

	// 7. resources（v2.6 P7-C，D5）：名→绝对路径/data URI；相对路径相对 theme.yaml 目录解析。
	// 渲染器据此把 ViewImage.ref 解码为位图（一次性缓存）。
	if len(t.Resources) > 0 {
		res := make(map[string]string, len(t.Resources))
		for name, ref := range t.Resources {
			// P7-E：按 isDark 选 light/dark 变体路径（单图写法两侧相同）。
			res[name] = resolveImagePath(ref.PathFor(isDark), themeFileDir)
		}
		rv.Resources = res
	}
	return rv, nil
}

// finalizePalette 把 v3 colors token 表解析为 ResolvedPalette：
//  1. derive（维度①）：未显式给的语义 token 由 primary 派生填入；
//  2. auto_dark（维度②，默认 false）：未显式给 dark 的 token 由 light 补；
//  3. 逐 token 在 isDark 环境下递归求值（LightDark.Select + ${} 多跳展开 + 循环保护）→ ParseColor；
//  4. 顶层语义便捷字段从 Tokens 镜像；toolbar_* token 填入 ResolvedToolbarPalette。
func finalizePalette(p *PaletteSchema, isDark bool) (ResolvedPalette, error) {
	// 在原始 token 表副本上做 derive / auto_dark（不污染 schema，便于明暗各自求值）。
	tokens := make(map[string]Color, len(p.Tokens)+1)
	maps.Copy(tokens, p.Tokens)

	if p.Derive.Enabled || (p.Derive.Algorithm != "" && p.Derive.Algorithm != "none") {
		algo := p.Derive.Algorithm
		if algo == "" {
			algo = "hct"
		}
		applyDeriveToTokens(tokens, p.Primary, algo)
	}
	if p.AutoDark {
		applyAutoDarkToTokens(tokens)
	}

	resolved, err := resolveColorTokens(tokens, p.Primary, isDark)
	if err != nil {
		return ResolvedPalette{}, err
	}

	out := ResolvedPalette{IsDark: isDark, Tokens: resolved}
	out.Primary = resolved["primary"]
	out.Bg = tokenOr(resolved, "bg")
	out.Surface = tokenOr(resolved, "surface")
	out.Border = tokenOr(resolved, "border")
	out.Text = tokenOr(resolved, "text")
	out.TextDim = tokenOr(resolved, "text_dim")
	out.TextHint = tokenOr(resolved, "text_hint")
	out.Accent = tokenOr(resolved, "accent")
	out.OnAccent = tokenOr(resolved, "on_accent")
	out.Shadow = tokenOr(resolved, "shadow")

	out.Toolbar = ResolvedToolbarPalette{
		Background:       tokenOrFallback(resolved, "toolbar_background", out.Bg),
		Border:           tokenOrFallback(resolved, "toolbar_border", out.Border),
		Grip:             tokenOrFallback(resolved, "toolbar_grip", out.TextHint),
		ModeChineseBg:    tokenOrFallback(resolved, "toolbar_mode_chinese_bg", out.Accent),
		ModeEnglishBg:    tokenOrFallback(resolved, "toolbar_mode_english_bg", out.TextDim),
		ModeText:         tokenOrFallback(resolved, "toolbar_mode_text", out.OnAccent),
		FullWidthOffBg:   tokenOrFallback(resolved, "toolbar_full_width_off_bg", out.Surface),
		FullWidthOffText: tokenOrFallback(resolved, "toolbar_full_width_off_text", out.TextDim),
		PunctEnglishBg:   tokenOrFallback(resolved, "toolbar_punct_english_bg", out.Surface),
		PunctEnglishText: tokenOrFallback(resolved, "toolbar_punct_english_text", out.TextDim),
		SettingsBg:       tokenOrFallback(resolved, "toolbar_settings_bg", out.Surface),
		SettingsIcon:     tokenOrFallback(resolved, "toolbar_settings_icon", out.Accent),
		SettingsHole:     tokenOrFallback(resolved, "toolbar_settings_hole", out.Surface),
	}

	return out, nil
}

// tokenOr 取已解析 token，缺失返回完全透明（顶层语义缺失视为未配置）。
func tokenOr(tokens map[string]color.Color, name string) color.Color {
	if c, ok := tokens[name]; ok {
		return c
	}
	return color.Transparent
}

// tokenOrFallback 取已解析 token，缺失用 fallback（toolbar_* 未显式给时回退顶层语义，保零回归）。
func tokenOrFallback(tokens map[string]color.Color, name string, fallback color.Color) color.Color {
	if c, ok := tokens[name]; ok {
		return c
	}
	return fallback
}

// parseColorOrTransparent 解析颜色字符串；"transparent"/空 字面值返回完全透明色
func parseColorOrTransparent(s string) color.Color {
	if s == "" || s == "transparent" {
		return color.Transparent
	}
	c, err := ParseHexColor(s)
	if err != nil {
		return color.Transparent
	}
	return c
}

// resolveImagePath data: URI 原样返回；相对路径拼到 paletteFileDir
func resolveImagePath(p string, baseDir string) string {
	if strings.HasPrefix(p, "data:") {
		return p
	}
	if filepath.IsAbs(p) {
		return p
	}
	return filepath.Join(baseDir, p)
}
