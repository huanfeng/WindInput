package theme

import (
	"encoding/json"
	"testing"

	"gopkg.in/yaml.v3"
)

// TestLightDark_UnmarshalYAML 验证标量/映射/缺一侧三种 YAML 写法的解析与回退。
func TestLightDark_UnmarshalYAML(t *testing.T) {
	t.Run("标量=明暗共用", func(t *testing.T) {
		var ld LightDark[string]
		if err := yaml.Unmarshal([]byte(`"#4285F4"`), &ld); err != nil {
			t.Fatal(err)
		}
		if ld.Light != "#4285F4" || ld.Dark != "#4285F4" {
			t.Errorf("标量应两侧相同, got %+v", ld)
		}
	})
	t.Run("映射=分设", func(t *testing.T) {
		var ld LightDark[string]
		if err := yaml.Unmarshal([]byte("{light: \"#FFFFFF\", dark: \"#2D2D2D\"}"), &ld); err != nil {
			t.Fatal(err)
		}
		if ld.Light != "#FFFFFF" || ld.Dark != "#2D2D2D" {
			t.Errorf("映射应分设, got %+v", ld)
		}
	})
	t.Run("映射缺dark→回退light", func(t *testing.T) {
		var ld LightDark[string]
		if err := yaml.Unmarshal([]byte(`{light: "#FFFFFF"}`), &ld); err != nil {
			t.Fatal(err)
		}
		if ld.Light != "#FFFFFF" || ld.Dark != "#FFFFFF" {
			t.Errorf("缺 dark 应回退 light, got %+v", ld)
		}
	})
	t.Run("映射缺light→回退dark", func(t *testing.T) {
		var ld LightDark[string]
		if err := yaml.Unmarshal([]byte(`{dark: "#2D2D2D"}`), &ld); err != nil {
			t.Fatal(err)
		}
		if ld.Light != "#2D2D2D" || ld.Dark != "#2D2D2D" {
			t.Errorf("缺 light 应回退 dark, got %+v", ld)
		}
	})
}

// TestLightDark_Select 验证变体选取与缺失侧回退。
func TestLightDark_Select(t *testing.T) {
	ld := LightDark[string]{Light: "L", Dark: "D"}
	if got := ld.Select(false); got != "L" {
		t.Errorf("Select(false)=%q, want L", got)
	}
	if got := ld.Select(true); got != "D" {
		t.Errorf("Select(true)=%q, want D", got)
	}
	// 单值（仅 light）：dark 回退 light。
	only := LightDark[string]{Light: "L"}
	if got := only.Select(true); got != "L" {
		t.Errorf("仅 light 时 Select(true)=%q, want L(回退)", got)
	}
	// 单值（仅 dark）：light 回退 dark。
	onlyD := LightDark[string]{Dark: "D"}
	if got := onlyD.Select(false); got != "D" {
		t.Errorf("仅 dark 时 Select(false)=%q, want D(回退)", got)
	}
}

// TestLightDark_IsZero 验证零值判定。
func TestLightDark_IsZero(t *testing.T) {
	var z LightDark[string]
	if !z.IsZero() {
		t.Error("零值 LightDark 应 IsZero=true")
	}
	if (LightDark[string]{Light: "x"}).IsZero() {
		t.Error("有值不应 IsZero")
	}
}

// TestLightDark_Marshal_RoundTrip 验证 Marshal 简洁性（共用→标量、分设→映射）+ 往返一致。
func TestLightDark_Marshal_RoundTrip(t *testing.T) {
	t.Run("YAML共用→标量", func(t *testing.T) {
		out, err := yaml.Marshal(NewLightDark("#FFF"))
		if err != nil {
			t.Fatal(err)
		}
		var back LightDark[string]
		if err := yaml.Unmarshal(out, &back); err != nil {
			t.Fatal(err)
		}
		if back.Light != "#FFF" || back.Dark != "#FFF" {
			t.Errorf("round-trip 失败: %s → %+v", out, back)
		}
	})
	t.Run("YAML分设→映射", func(t *testing.T) {
		ld := LightDark[string]{Light: "#FFF", Dark: "#000"}
		out, err := yaml.Marshal(ld)
		if err != nil {
			t.Fatal(err)
		}
		var back LightDark[string]
		if err := yaml.Unmarshal(out, &back); err != nil {
			t.Fatal(err)
		}
		if back != ld {
			t.Errorf("round-trip 失败: %s → %+v", out, back)
		}
	})
	t.Run("JSON共用→标量+往返", func(t *testing.T) {
		out, err := json.Marshal(NewLightDark("#FFF"))
		if err != nil {
			t.Fatal(err)
		}
		if string(out) != `"#FFF"` {
			t.Errorf("共用应序列化为标量, got %s", out)
		}
		var back LightDark[string]
		if err := json.Unmarshal(out, &back); err != nil {
			t.Fatal(err)
		}
		if back.Light != "#FFF" || back.Dark != "#FFF" {
			t.Errorf("JSON round-trip 失败: %+v", back)
		}
	})
	t.Run("JSON分设→映射+往返", func(t *testing.T) {
		ld := LightDark[string]{Light: "#FFF", Dark: "#000"}
		out, err := json.Marshal(ld)
		if err != nil {
			t.Fatal(err)
		}
		var back LightDark[string]
		if err := json.Unmarshal(out, &back); err != nil {
			t.Fatal(err)
		}
		if back != ld {
			t.Errorf("JSON round-trip 失败: %s → %+v", out, back)
		}
	})
}
