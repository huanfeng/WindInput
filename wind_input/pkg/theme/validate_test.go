package theme

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// validate_test.go — V3-D 加载期校验（决策 11）单测：未知 token / 缺失 image.ref /
// 循环引用 / 非法颜色字面 各 fail fast；合法引用放行。

// TestValidateViews_UnknownToken 直接校验：views 节点引用 colors 表不存在的 token → 报错。
func TestValidateViews_UnknownToken(t *testing.T) {
	pal := testPalette()
	views := &Views{
		Window: ViewNode{Background: ViewFill{Color: NewLightDark("${nope}")}},
	}
	err := validateViews(views, pal.Tokens, nil)
	if err == nil || !strings.Contains(err.Error(), "未知颜色 token") {
		t.Fatalf("应报未知 token 错, got %v", err)
	}
	if !strings.Contains(err.Error(), "window.background.color") {
		t.Errorf("错误应含节点路径, got %v", err)
	}
}

// TestValidateViews_UnknownTokenInState 校验：states 递归节点的未知 token 也被捕获。
func TestValidateViews_UnknownTokenInState(t *testing.T) {
	pal := testPalette()
	views := &Views{
		Item: ViewNode{Selected: &ViewNode{Color: NewLightDark("${typo_sel}")}},
	}
	err := validateViews(views, pal.Tokens, nil)
	if err == nil || !strings.Contains(err.Error(), "item.selected.color") {
		t.Fatalf("应捕获 states 递归节点未知 token, got %v", err)
	}
}

// TestValidateViews_MissingImageRef 校验：image.ref 既不在 resources、也非合法 path/data URI → 报错。
func TestValidateViews_MissingImageRef(t *testing.T) {
	pal := testPalette()
	views := &Views{
		Window: ViewNode{Background: ViewFill{Image: &ViewImage{Ref: "missing"}}},
	}
	err := validateViews(views, pal.Tokens, nil)
	if err == nil || !strings.Contains(err.Error(), "未知图片 ref") {
		t.Fatalf("应报未知 image ref 错, got %v", err)
	}
}

// TestValidateViews_ValidRefs 校验：resources 命中 / data URI / 字面路径 / hex / transparent 均放行。
func TestValidateViews_ValidRefs(t *testing.T) {
	pal := testPalette()
	resources := map[string]ResourceRef{"panel": {Light: "panel.png", Dark: "panel.png"}}
	views := &Views{
		Window:     ViewNode{Background: ViewFill{Color: NewLightDark("${bg}"), Image: &ViewImage{Ref: "panel"}}},
		PreeditBar: ViewNode{Background: ViewFill{Color: NewLightDark("transparent")}, Color: NewLightDark("#112233")},
		Item:       ViewNode{Background: ViewFill{Image: &ViewImage{Ref: "data:image/png;base64,xxx"}}},
		Index:      ViewNode{Background: ViewFill{Image: &ViewImage{Ref: "skins/foo.png"}}},
	}
	if err := validateViews(views, pal.Tokens, resources); err != nil {
		t.Fatalf("合法引用不应报错, got %v", err)
	}
}

// TestValidateViews_BadHexLiteral 校验：非 ${}/transparent 的颜色字面须是合法 hex。
func TestValidateViews_BadHexLiteral(t *testing.T) {
	pal := testPalette()
	views := &Views{Window: ViewNode{Color: NewLightDark("reddish")}}
	err := validateViews(views, pal.Tokens, nil)
	if err == nil || !strings.Contains(err.Error(), "颜色字面值非法") {
		t.Fatalf("应报非法颜色字面, got %v", err)
	}
}

// TestLoadTheme_UnknownToken_FailFast 端到端：含未知 token 的主题经 ResolveV3 报错（不进入渲染）。
func TestLoadTheme_UnknownToken_FailFast(t *testing.T) {
	tmp, cleanup := setupTestThemes(t)
	defer cleanup()
	m := makeTestManager(tmp)

	bad := `meta: {name: "bad-token"}
base: test-base
views:
  window:
    background: {color: "${does_not_exist}"}
`
	dir := filepath.Join(tmp, "bad-token")
	_ = os.MkdirAll(dir, 0o755)
	_ = os.WriteFile(filepath.Join(dir, "theme.yaml"), []byte(bad), 0o644)

	th := loadMerged(t, m, "bad-token")
	if _, err := m.ResolveV3(th, false, dir); err == nil {
		t.Fatal("含未知 token 的主题应 ResolveV3 报错")
	}
}

// TestLoadTheme_MissingImageRef_FailFast 端到端：image.ref 不在 resources → 报错。
func TestLoadTheme_MissingImageRef_FailFast(t *testing.T) {
	tmp, cleanup := setupTestThemes(t)
	defer cleanup()
	m := makeTestManager(tmp)

	bad := `meta: {name: "bad-ref"}
base: test-base
views:
  window:
    background: {image: {ref: "ghost"}}
`
	dir := filepath.Join(tmp, "bad-ref")
	_ = os.MkdirAll(dir, 0o755)
	_ = os.WriteFile(filepath.Join(dir, "theme.yaml"), []byte(bad), 0o644)

	th := loadMerged(t, m, "bad-ref")
	if _, err := m.ResolveV3(th, false, dir); err == nil {
		t.Fatal("缺失 image.ref 的主题应 ResolveV3 报错")
	}
}

// TestLoadTheme_CircularColorRef_FailFast 端到端：colors token 互相引用成环 → 报错（非静默）。
func TestLoadTheme_CircularColorRef_FailFast(t *testing.T) {
	tmp, cleanup := setupTestThemes(t)
	defer cleanup()
	m := makeTestManager(tmp)

	bad := `meta: {name: "cycle"}
colors:
  primary: "#4285F4"
  bg:   "${surface}"
  surface: "${bg}"
`
	dir := filepath.Join(tmp, "cycle")
	_ = os.MkdirAll(dir, 0o755)
	_ = os.WriteFile(filepath.Join(dir, "theme.yaml"), []byte(bad), 0o644)

	th := loadMerged(t, m, "cycle")
	_, err := m.ResolveV3(th, false, dir)
	if err == nil || !strings.Contains(err.Error(), "成环") {
		t.Fatalf("成环颜色引用应报错, got %v", err)
	}
}
