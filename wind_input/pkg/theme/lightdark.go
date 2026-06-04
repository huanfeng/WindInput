package theme

import (
	"encoding/json"

	"gopkg.in/yaml.v3"
)

// LightDark[T] 是主题 v3 的亮暗原语：单值=明暗共用，{light,dark}=分设。
//
// 设计要点（见 docs/design/theme-schema-v3.md「原语类型」）：
//   - 仅作用于**表现层值**（颜色 ColorScalar、图片 ref，实例 T 均为 string）；
//     几何/结构不随亮暗变化（Dimension 不是 LightDark）。
//   - 命名 LightDark（而非泛化 Variant）以诚实表达「仅亮暗二值轴」——将来若需
//     @2x / 高对比度 / 平台等其它轴，再抽象一层通用 Conditional[T]（现在不预先泛化）。
//   - 与 ResourceRef（P7-E 的图片 {light,dark}）同构，是其泛型化推广；颜色 v3 化即「追上图片」。
//
// YAML / JSON 均支持两种写法：
//   - 标量："#4285F4"（明暗共用）
//   - 映射：{light: "#FFFFFF", dark: "#2D2D2D"}（缺一侧回退另一侧）
type LightDark[T comparable] struct {
	Light T
	Dark  T
}

// NewLightDark 构造明暗共用的单值 LightDark（测试/默认值用）。
func NewLightDark[T comparable](v T) LightDark[T] {
	return LightDark[T]{Light: v, Dark: v}
}

// Select 按是否暗色返回对应分支；缺失侧（零值）回退另一侧（保证单值写法可用）。
func (ld LightDark[T]) Select(isDark bool) T {
	var zero T
	if isDark {
		if ld.Dark != zero {
			return ld.Dark
		}
		return ld.Light
	}
	if ld.Light != zero {
		return ld.Light
	}
	return ld.Dark
}

// IsZero 报告两侧是否均为零值（用于「未指定→保留默认」判定）。
func (ld LightDark[T]) IsZero() bool {
	var zero T
	return ld.Light == zero && ld.Dark == zero
}

// normalize 单侧为零值时回退另一侧（映射只写一侧时仍可用）。
func (ld *LightDark[T]) normalize() {
	var zero T
	if ld.Light == zero {
		ld.Light = ld.Dark
	}
	if ld.Dark == zero {
		ld.Dark = ld.Light
	}
}

func (ld *LightDark[T]) UnmarshalYAML(value *yaml.Node) error {
	if value.Kind == yaml.ScalarNode {
		var v T
		if err := value.Decode(&v); err != nil {
			return err
		}
		ld.Light, ld.Dark = v, v
		return nil
	}
	var m struct {
		Light T `yaml:"light"`
		Dark  T `yaml:"dark"`
	}
	if err := value.Decode(&m); err != nil {
		return err
	}
	ld.Light, ld.Dark = m.Light, m.Dark
	ld.normalize()
	return nil
}

// MarshalYAML：light==dark 输出标量（主题文件保持简洁），否则输出 {light,dark}。
func (ld LightDark[T]) MarshalYAML() (any, error) {
	if ld.Light == ld.Dark {
		return ld.Light, nil
	}
	return map[string]T{"light": ld.Light, "dark": ld.Dark}, nil
}

func (ld *LightDark[T]) UnmarshalJSON(data []byte) error {
	var v T
	if err := json.Unmarshal(data, &v); err == nil {
		ld.Light, ld.Dark = v, v
		return nil
	}
	var m struct {
		Light T `json:"light"`
		Dark  T `json:"dark"`
	}
	if err := json.Unmarshal(data, &m); err != nil {
		return err
	}
	ld.Light, ld.Dark = m.Light, m.Dark
	ld.normalize()
	return nil
}

// MarshalJSON：light==dark 输出标量，否则输出 {light,dark}。
func (ld LightDark[T]) MarshalJSON() ([]byte, error) {
	if ld.Light == ld.Dark {
		return json.Marshal(ld.Light)
	}
	return json.Marshal(map[string]T{"light": ld.Light, "dark": ld.Dark})
}
