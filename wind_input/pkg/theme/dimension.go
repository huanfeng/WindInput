package theme

import (
	"encoding/json"
	"fmt"
	"strconv"
	"strings"

	"gopkg.in/yaml.v3"
)

// Dimension 是带单位的几何尺寸（v2.6 单位扩展）。
//
// 两种单位：
//   - dp（密度无关像素，默认）：随 DPI scale 缩放，适合间距/圆角/字号等"物理尺寸感一致"的量。
//   - px（设备像素）：不缩放，恒为该设备像素数，适合发丝线（1px 边框/分隔线）等"无论多高 DPI 都细如一线"的量。
//
// YAML/JSON 表示（联合，向后兼容）：
//   - 裸数字 `8`        → dp（旧主题零影响）
//   - 字符串 `"8dp"`    → dp（显式）
//   - 字符串 `"1px"`    → px（设备像素，不缩放）
//
// 序列化：dp 输出裸整数（保持旧主题习惯与 diff 友好），px 输出 `"Npx"`。
type Dimension struct {
	Value int
	Px    bool // true=设备像素(不缩放); false=dp(×scale)
}

// Dp 构造一个 dp 尺寸（缩放）。
func Dp(v int) Dimension { return Dimension{Value: v, Px: false} }

// PxDim 构造一个 px 尺寸（不缩放）。
func PxDim(v int) Dimension { return Dimension{Value: v, Px: true} }

// Scaled 按 DPI scale 把逻辑尺寸换算为设备像素：px 单位原样返回，dp 单位四舍五入 ×scale。
func (d Dimension) Scaled(scale float64) int {
	if d.Px {
		return d.Value
	}
	return int(float64(d.Value)*scale + 0.5)
}

// parseDimension 解析标量形态：裸整数→dp；"Npx"→px；"Ndp"/"N"→dp。
func parseDimension(raw string) (Dimension, error) {
	s := strings.TrimSpace(raw)
	if s == "" {
		return Dimension{}, fmt.Errorf("空尺寸值")
	}
	unit := false // px?
	switch {
	case strings.HasSuffix(s, "px"):
		unit = true
		s = strings.TrimSpace(strings.TrimSuffix(s, "px"))
	case strings.HasSuffix(s, "dp"):
		s = strings.TrimSpace(strings.TrimSuffix(s, "dp"))
	}
	n, err := strconv.Atoi(s)
	if err != nil {
		return Dimension{}, fmt.Errorf("无效尺寸值 %q: %w", raw, err)
	}
	return Dimension{Value: n, Px: unit}, nil
}

// UnmarshalYAML 接受标量整数（dp）或字符串（"Npx"/"Ndp"）。
func (d *Dimension) UnmarshalYAML(node *yaml.Node) error {
	var n int
	if err := node.Decode(&n); err == nil {
		d.Value, d.Px = n, false
		return nil
	}
	var s string
	if err := node.Decode(&s); err != nil {
		return fmt.Errorf("尺寸值须为整数或带单位字符串: %w", err)
	}
	parsed, err := parseDimension(s)
	if err != nil {
		return err
	}
	*d = parsed
	return nil
}

// MarshalYAML：dp 输出裸整数（向后兼容），px 输出 "Npx"。
func (d Dimension) MarshalYAML() (any, error) {
	if d.Px {
		return strconv.Itoa(d.Value) + "px", nil
	}
	return d.Value, nil
}

// UnmarshalJSON 接受数字（dp）或字符串（"Npx"/"Ndp"）。
func (d *Dimension) UnmarshalJSON(data []byte) error {
	var n int
	if err := json.Unmarshal(data, &n); err == nil {
		d.Value, d.Px = n, false
		return nil
	}
	var s string
	if err := json.Unmarshal(data, &s); err != nil {
		return fmt.Errorf("尺寸值须为数字或带单位字符串: %w", err)
	}
	parsed, err := parseDimension(s)
	if err != nil {
		return err
	}
	*d = parsed
	return nil
}

// MarshalJSON：dp 输出数字，px 输出 "Npx" 字符串。
func (d Dimension) MarshalJSON() ([]byte, error) {
	if d.Px {
		return json.Marshal(strconv.Itoa(d.Value) + "px")
	}
	return json.Marshal(d.Value)
}
