package theme

import (
	"encoding/json"
	"fmt"
	"strconv"
	"strings"

	"gopkg.in/yaml.v3"
)

// Dimension 是带单位的几何尺寸。
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

// OffsetValue 覆盖图/背景图的偏移分量：dp（随 DPI 缩放）或百分比（相对 host 对应边长）。
//
// 百分比是「相对量」——须知 host 宽/高才能换算，故百分比分量延迟到 paint 阶段（host 已知）解析；
// dp 分量可在 build 阶段经 scale 预算为像素。两者互斥：一个 OffsetValue 要么 dp、要么百分比。
//
// YAML/JSON 表示（向后兼容旧 offset:{x:-8} 裸数字）：
//   - 裸数字 -8 / "-8dp" → dp
//   - "-10%"            → 百分比（相对 host 宽或高，正负皆可）
type OffsetValue struct {
	DP    int     // dp 偏移（!IsPct 时有效）
	Pct   float64 // 百分比偏移（IsPct 时有效）
	IsPct bool
}

// OffsetDp 构造一个 dp 偏移。
func OffsetDp(v int) OffsetValue { return OffsetValue{DP: v} }

// OffsetPct 构造一个百分比偏移（相对 host 对应边长）。
func OffsetPct(v float64) OffsetValue { return OffsetValue{Pct: v, IsPct: true} }

// Split 把偏移拆为 (dp, pct)：百分比时 dp=0、pct=值；否则 dp=值、pct=0。供 toRVImage 分流到
// RVImage 的 OffsetX(dp，build 阶段 ×scale) 与 OffsetXPct(百分比，paint 阶段相对 host 换算)。
func (o OffsetValue) Split() (int, float64) {
	if o.IsPct {
		return 0, o.Pct
	}
	return o.DP, 0
}

func parseOffsetValue(raw string) (OffsetValue, error) {
	s := strings.TrimSpace(raw)
	if s == "" {
		return OffsetValue{}, nil
	}
	if strings.HasSuffix(s, "%") {
		n, err := strconv.ParseFloat(strings.TrimSpace(strings.TrimSuffix(s, "%")), 64)
		if err != nil {
			return OffsetValue{}, fmt.Errorf("无效百分比偏移 %q: %w", raw, err)
		}
		return OffsetValue{Pct: n, IsPct: true}, nil
	}
	s = strings.TrimSpace(strings.TrimSuffix(s, "dp"))
	n, err := strconv.Atoi(s)
	if err != nil {
		return OffsetValue{}, fmt.Errorf("无效偏移值 %q: %w", raw, err)
	}
	return OffsetValue{DP: n}, nil
}

// UnmarshalYAML 接受标量整数（dp）或字符串（"N%"/"Ndp"）。
func (o *OffsetValue) UnmarshalYAML(node *yaml.Node) error {
	var n int
	if err := node.Decode(&n); err == nil {
		*o = OffsetValue{DP: n}
		return nil
	}
	var s string
	if err := node.Decode(&s); err != nil {
		return fmt.Errorf("偏移值须为整数或带单位字符串: %w", err)
	}
	parsed, err := parseOffsetValue(s)
	if err != nil {
		return err
	}
	*o = parsed
	return nil
}

// MarshalYAML：dp 输出裸整数（向后兼容），百分比输出 "N%"。
func (o OffsetValue) MarshalYAML() (any, error) {
	if o.IsPct {
		return strconv.FormatFloat(o.Pct, 'f', -1, 64) + "%", nil
	}
	return o.DP, nil
}

// UnmarshalJSON 接受数字（dp）或字符串（"N%"/"Ndp"）。
func (o *OffsetValue) UnmarshalJSON(data []byte) error {
	var n int
	if err := json.Unmarshal(data, &n); err == nil {
		*o = OffsetValue{DP: n}
		return nil
	}
	var s string
	if err := json.Unmarshal(data, &s); err != nil {
		return fmt.Errorf("偏移值须为数字或带单位字符串: %w", err)
	}
	parsed, err := parseOffsetValue(s)
	if err != nil {
		return err
	}
	*o = parsed
	return nil
}

// MarshalJSON：dp 输出数字，百分比输出 "N%" 字符串。
func (o OffsetValue) MarshalJSON() ([]byte, error) {
	if o.IsPct {
		return json.Marshal(strconv.FormatFloat(o.Pct, 'f', -1, 64) + "%")
	}
	return json.Marshal(o.DP)
}
