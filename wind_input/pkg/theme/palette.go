package theme

import (
	"fmt"

	"gopkg.in/yaml.v3"
)

// Color 是 v3 颜色 token 值：单值=明暗共用，{light,dark}=分设（见 lightdark.go）。
// ColorScalar 形态："#RRGGBB[AA]" / "${tokenName}" / "transparent"。
type Color = LightDark[string]

// PaletteSchema 描述主题的颜色 token 提供者（v3「colors」块，见 docs/design/theme-schema-v3.md）。
// 颜色全部扁平进 Tokens（值为 LightDark），不再有 light/dark 顶层分块、不再有嵌套窗口色组。
//
// 命名约定（落地细则）：
//   - 顶层语义：primary / bg / surface / border / text / text_dim / text_hint / accent / on_accent / shadow
//   - 候选窗特有色提升为语义：selection / selection_text / hover
//   - 其它窗口特有色用功能前缀 token：menu_* / tooltip_* / status_* / toast_* / toolbar_*
type PaletteSchema struct {
	Meta     PaletteMeta      `yaml:"meta" json:"meta"`
	Primary  string           `yaml:"primary" json:"primary"`
	Derive   DeriveConfig     `yaml:"derive" json:"derive"`
	AutoDark bool             `yaml:"auto_dark" json:"auto_dark"` // 维度②：未显式给 dark 的 token 由 light 派生（默认 false）
	Tokens   map[string]Color `yaml:"-" json:"tokens"`            // 全部扁平颜色 token（自定义 UnmarshalYAML 填充）
}

// PaletteMeta 标识一个共享 palette 零件
type PaletteMeta struct {
	Name    string `yaml:"name" json:"name"`
	Version string `yaml:"version" json:"version"`
}

// DeriveConfig 颜色派生配置（维度①：由 primary 派生缺失的语义色）
type DeriveConfig struct {
	Enabled   bool   `yaml:"enabled" json:"enabled"`
	Algorithm string `yaml:"algorithm" json:"algorithm"` // hct | hsl-shift | none
}

// 这些键在 colors 块里是特殊字段，不进 Tokens 表。
var paletteReservedKeys = map[string]bool{
	"meta":      true,
	"primary":   true,
	"derive":    true,
	"auto_dark": true,
}

// UnmarshalYAML 把 colors 块解析为 v3 PaletteSchema：
// 遍历 mapping，meta/primary/derive/auto_dark 抽出特殊处理，其余键 → Tokens（每值按 LightDark 解析）。
func (p *PaletteSchema) UnmarshalYAML(value *yaml.Node) error {
	if value.Kind != yaml.MappingNode {
		return fmt.Errorf("palette/colors 必须为 mapping，got kind=%d", value.Kind)
	}
	p.Tokens = make(map[string]Color)
	// mapping 子节点成对出现：key, value, key, value, ...
	for i := 0; i+1 < len(value.Content); i += 2 {
		keyNode := value.Content[i]
		valNode := value.Content[i+1]
		key := keyNode.Value
		switch key {
		case "meta":
			if err := valNode.Decode(&p.Meta); err != nil {
				return fmt.Errorf("colors.meta: %w", err)
			}
		case "primary":
			if err := valNode.Decode(&p.Primary); err != nil {
				return fmt.Errorf("colors.primary: %w", err)
			}
		case "derive":
			if err := valNode.Decode(&p.Derive); err != nil {
				return fmt.Errorf("colors.derive: %w", err)
			}
		case "auto_dark":
			if err := valNode.Decode(&p.AutoDark); err != nil {
				return fmt.Errorf("colors.auto_dark: %w", err)
			}
		default:
			if paletteReservedKeys[key] {
				continue
			}
			var c Color
			if err := valNode.Decode(&c); err != nil {
				return fmt.Errorf("colors.%s: %w", key, err)
			}
			p.Tokens[key] = c
		}
	}
	return nil
}

// MarshalYAML 把 v3 PaletteSchema 还原为扁平 colors mapping（内联/导出用）。
// 输出顺序：meta、primary、derive、auto_dark（仅 true 时）、再各 token（map 顺序由 yaml 库排序）。
func (p PaletteSchema) MarshalYAML() (any, error) {
	out := map[string]any{}
	if p.Meta.Name != "" || p.Meta.Version != "" {
		out["meta"] = p.Meta
	}
	if p.Primary != "" {
		out["primary"] = p.Primary
	}
	if p.Derive.Enabled || p.Derive.Algorithm != "" {
		out["derive"] = p.Derive
	}
	if p.AutoDark {
		out["auto_dark"] = p.AutoDark
	}
	for k, v := range p.Tokens {
		out[k] = v
	}
	return out, nil
}
