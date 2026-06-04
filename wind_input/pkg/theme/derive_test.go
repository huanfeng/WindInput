package theme

import "testing"

func TestDeriveHSLShift_Light(t *testing.T) {
	d := derivePalette("#4285F4", "hsl-shift", false)
	if d.Bg != "#FFFFFF" {
		t.Errorf("light bg want #FFFFFF, got %s", d.Bg)
	}
	if d.Accent != "#4285F4" {
		t.Errorf("accent should equal primary, got %s", d.Accent)
	}
	if d.OnAccent != "#FFFFFF" {
		t.Errorf("on_accent for bright blue want #FFFFFF, got %s", d.OnAccent)
	}
	if d.Text == "" || d.Border == "" || d.Surface == "" {
		t.Errorf("derive should fill all semantic fields, got %+v", d)
	}
}

func TestDeriveHSLShift_Dark(t *testing.T) {
	d := derivePalette("#4285F4", "hsl-shift", true)
	if d.Bg == "#FFFFFF" {
		t.Errorf("dark bg should not be white, got %s", d.Bg)
	}
	if d.Text == "" {
		t.Errorf("dark text should be filled")
	}
}

func TestDerivePicksContrastOnColor(t *testing.T) {
	// 亮色主题色 → on_accent 应为黑
	d := derivePalette("#FFFF66", "hsl-shift", false)
	if d.OnAccent != "#000000" {
		t.Errorf("bright primary should yield on_accent=#000000, got %s", d.OnAccent)
	}
}

func TestDeriveNone(t *testing.T) {
	d := derivePalette("#4285F4", "none", false)
	if d.Accent != "" || d.Bg != "" {
		t.Errorf("none algorithm should return empty struct, got %+v", d)
	}
}

// TestApplyDeriveToTokens_PreservesUserValues：用户显式 token 不被派生覆盖，缺失 token 被填充。
func TestApplyDeriveToTokens_PreservesUserValues(t *testing.T) {
	tokens := map[string]Color{
		"bg": {Light: "#ABCDEF", Dark: "#123456"}, // 用户显式
		// accent 缺失 → 待派生
	}
	applyDeriveToTokens(tokens, "#4285F4", "hsl-shift")
	if tokens["bg"].Light != "#ABCDEF" {
		t.Errorf("user bg.light should be preserved, got %s", tokens["bg"].Light)
	}
	if tokens["accent"].Light != "#4285F4" {
		t.Errorf("missing accent should be filled by derive, got %s", tokens["accent"].Light)
	}
}

// TestResolveColorTokens_SimpleChain：${} 多跳展开 + LightDark 选取。
func TestResolveColorTokens_SimpleChain(t *testing.T) {
	tokens := map[string]Color{
		"bg":      {Light: "#FFFFFF", Dark: "#FFFFFF"},
		"accent":  {Light: "${primary}", Dark: "${primary}"},
		"surface": {Light: "${accent}", Dark: "${accent}"}, // 两跳 → primary
	}
	out, err := resolveColorTokens(tokens, "#4285F4", false)
	if err != nil {
		t.Fatalf("resolve failed: %v", err)
	}
	if ColorToHexRGB(out["accent"]) != "#4285F4" {
		t.Errorf("accent want #4285F4, got %s", ColorToHexRGB(out["accent"]))
	}
	if ColorToHexRGB(out["surface"]) != "#4285F4" {
		t.Errorf("surface (两跳) want #4285F4, got %s", ColorToHexRGB(out["surface"]))
	}
	if ColorToHexRGB(out["bg"]) != "#FFFFFF" {
		t.Errorf("bg want #FFFFFF, got %s", ColorToHexRGB(out["bg"]))
	}
}

// TestResolveColorTokens_LightDark：isDark 贯穿求值，逐 token 选分支。
func TestResolveColorTokens_LightDark(t *testing.T) {
	tokens := map[string]Color{
		"bg": {Light: "#FFFFFF", Dark: "#2D2D2D"},
	}
	light, _ := resolveColorTokens(tokens, "#4285F4", false)
	dark, _ := resolveColorTokens(tokens, "#4285F4", true)
	if ColorToHexRGB(light["bg"]) != "#FFFFFF" {
		t.Errorf("light bg want #FFFFFF, got %s", ColorToHexRGB(light["bg"]))
	}
	if ColorToHexRGB(dark["bg"]) != "#2D2D2D" {
		t.Errorf("dark bg want #2D2D2D, got %s", ColorToHexRGB(dark["bg"]))
	}
}

func TestResolveColorTokens_TransparentLiteral(t *testing.T) {
	tokens := map[string]Color{"x": {Light: "transparent", Dark: "transparent"}}
	out, err := resolveColorTokens(tokens, "#4285F4", false)
	if err != nil {
		t.Fatalf("resolve failed: %v", err)
	}
	if _, _, _, a := out["x"].RGBA(); a != 0 {
		t.Errorf("transparent should yield alpha 0, got %d", a)
	}
}

func TestResolveColorTokens_UnknownToken(t *testing.T) {
	tokens := map[string]Color{"accent": {Light: "${nosuch}", Dark: "${nosuch}"}}
	if _, err := resolveColorTokens(tokens, "#4285F4", false); err == nil {
		t.Errorf("expected error for unknown token")
	}
}

func TestResolveColorTokens_Cycle(t *testing.T) {
	tokens := map[string]Color{
		"a": {Light: "${b}", Dark: "${b}"},
		"b": {Light: "${a}", Dark: "${a}"},
	}
	if _, err := resolveColorTokens(tokens, "#4285F4", false); err == nil {
		t.Errorf("expected error for cyclic token reference")
	}
}

func TestResolveColorTokens_PrimaryRefRejected(t *testing.T) {
	if _, err := resolveColorTokens(map[string]Color{}, "${primary}", false); err == nil {
		t.Errorf("primary itself must not contain ${} ref")
	}
}
