package theme

import (
	"encoding/json"
	"testing"

	"gopkg.in/yaml.v3"
)

func TestDimension_Scaled(t *testing.T) {
	// dp 随 scale 缩放（四舍五入）
	if got := Dp(1).Scaled(1.5); got != 2 {
		t.Errorf("dp 1 @1.5 应=2(round), got %d", got)
	}
	if got := Dp(8).Scaled(2.0); got != 16 {
		t.Errorf("dp 8 @2.0 应=16, got %d", got)
	}
	// px 不缩放（发丝线恒为设备像素）
	if got := PxDim(1).Scaled(1.5); got != 1 {
		t.Errorf("px 1 @1.5 应=1(不缩放), got %d", got)
	}
	if got := PxDim(1).Scaled(2.0); got != 1 {
		t.Errorf("px 1 @2.0 应=1(不缩放), got %d", got)
	}
}

func TestDimension_YAMLUnion(t *testing.T) {
	cases := []struct {
		src    string
		val    int
		px     bool
		expect string // re-marshal 期望
	}{
		{"8", 8, false, "8\n"},
		{"1px", 1, true, "1px\n"},
		{"2dp", 2, false, "2\n"}, // dp 序列化回裸数字
	}
	for _, c := range cases {
		var d Dimension
		if err := yaml.Unmarshal([]byte(c.src), &d); err != nil {
			t.Fatalf("%q 解析失败: %v", c.src, err)
		}
		if d.Value != c.val || d.Px != c.px {
			t.Errorf("%q 解析错: got {%d, px=%v}, 期望 {%d, px=%v}", c.src, d.Value, d.Px, c.val, c.px)
		}
		out, err := yaml.Marshal(d)
		if err != nil {
			t.Fatalf("%q 序列化失败: %v", c.src, err)
		}
		if string(out) != c.expect {
			t.Errorf("%q 序列化错: got %q, 期望 %q", c.src, string(out), c.expect)
		}
	}
}

func TestDimension_JSONUnion(t *testing.T) {
	var d Dimension
	if err := json.Unmarshal([]byte(`"1px"`), &d); err != nil {
		t.Fatalf("JSON 解析 1px 失败: %v", err)
	}
	if d.Value != 1 || !d.Px {
		t.Errorf("JSON 1px 解析错: %+v", d)
	}
	if err := json.Unmarshal([]byte(`8`), &d); err != nil {
		t.Fatalf("JSON 解析裸数字失败: %v", err)
	}
	if d.Value != 8 || d.Px {
		t.Errorf("JSON 8 应=dp 8: %+v", d)
	}
	// px round-trip 为字符串，dp round-trip 为数字
	if out, _ := json.Marshal(PxDim(1)); string(out) != `"1px"` {
		t.Errorf("px JSON 序列化应=\"1px\", got %s", out)
	}
	if out, _ := json.Marshal(Dp(8)); string(out) != `8` {
		t.Errorf("dp JSON 序列化应=8, got %s", out)
	}
}
