package theme

import (
	"testing"

	"gopkg.in/yaml.v3"
)

func TestDefaultBehavior(t *testing.T) {
	b := defaultBehavior()
	if b.FontSize != 18 {
		t.Errorf("FontSize 默认应为 18, got %d", b.FontSize)
	}
	if b.AlwaysShowPager {
		t.Errorf("AlwaysShowPager 默认应为 false, got %v", b.AlwaysShowPager)
	}
	if b.ShowPageNumber != true {
		t.Errorf("ShowPageNumber 默认应为 true, got %v", b.ShowPageNumber)
	}
	if b.VerticalMaxWidth != 600 {
		t.Errorf("VerticalMaxWidth 默认应为 600, got %d", b.VerticalMaxWidth)
	}
}

func TestMergeBehavior_NilKeepsBase(t *testing.T) {
	base := defaultBehavior()
	got := mergeBehavior(base, nil)
	if got != base {
		t.Errorf("nil override 应原样返回基线, got %+v", got)
	}
}

func TestMergeBehavior_PartialOverride(t *testing.T) {
	base := defaultBehavior()
	fs := 22
	alwaysShowPager := true
	ov := &Behavior{FontSize: &fs, AlwaysShowPager: &alwaysShowPager}
	got := mergeBehavior(base, ov)
	if got.FontSize != 22 {
		t.Errorf("FontSize 应被覆盖为 22, got %d", got.FontSize)
	}
	if got.AlwaysShowPager != true {
		t.Errorf("AlwaysShowPager 应被覆盖为 true, got %v", got.AlwaysShowPager)
	}
	if got.ShowPageNumber != true || got.VerticalMaxWidth != 600 {
		t.Errorf("未覆盖字段应保持基线, got ShowPageNumber=%v VerticalMaxWidth=%d", got.ShowPageNumber, got.VerticalMaxWidth)
	}
}

func TestMergeBehavior_FullOverride(t *testing.T) {
	base := defaultBehavior()
	fs := 24
	asp := true
	spn := false
	vmw := 800
	ov := &Behavior{
		FontSize:         &fs,
		AlwaysShowPager:  &asp,
		ShowPageNumber:   &spn,
		VerticalMaxWidth: &vmw,
	}
	got := mergeBehavior(base, ov)
	if got.FontSize != 24 {
		t.Errorf("FontSize 应被覆盖为 24, got %d", got.FontSize)
	}
	if got.AlwaysShowPager != true {
		t.Errorf("AlwaysShowPager 应被覆盖为 true, got %v", got.AlwaysShowPager)
	}
	if got.ShowPageNumber != false {
		t.Errorf("ShowPageNumber 应被覆盖为 false, got %v", got.ShowPageNumber)
	}
	if got.VerticalMaxWidth != 800 {
		t.Errorf("VerticalMaxWidth 应被覆盖为 800, got %d", got.VerticalMaxWidth)
	}
}

func TestBehavior_YAMLParse(t *testing.T) {
	src := []byte("font_size: 16\nshow_page_number: false\n")
	var b Behavior
	if err := yaml.Unmarshal(src, &b); err != nil {
		t.Fatalf("yaml 解析失败: %v", err)
	}
	if b.FontSize == nil || *b.FontSize != 16 {
		t.Errorf("font_size 应解析为 16, got %v", b.FontSize)
	}
	if b.ShowPageNumber == nil || *b.ShowPageNumber != false {
		t.Errorf("show_page_number 应解析为 false, got %v", b.ShowPageNumber)
	}
	if b.AlwaysShowPager != nil {
		t.Errorf("未写的 always_show_pager 应为 nil, got %v", b.AlwaysShowPager)
	}
}

func TestResolveV25_BehaviorDefault(t *testing.T) {
	tmp, cleanup := setupTestThemes(t)
	defer cleanup()
	m := makeTestManager(tmp)
	th := loadMerged(t, m, "test-base")
	rv, err := m.ResolveV25(th, false, tmp)
	if err != nil {
		t.Fatalf("ResolveV25: %v", err)
	}
	if rv.Behavior != defaultBehavior() {
		t.Errorf("未提供 behavior 时应为 defaultBehavior, got %+v", rv.Behavior)
	}
}

func TestResolveV25_BehaviorOverride(t *testing.T) {
	tmp, cleanup := setupTestThemes(t)
	defer cleanup()
	m := makeTestManager(tmp)
	fs := 20
	th := loadMerged(t, m, "test-base")
	th.Behavior = &Behavior{FontSize: &fs}
	rv, err := m.ResolveV25(th, false, tmp)
	if err != nil {
		t.Fatalf("ResolveV25: %v", err)
	}
	if rv.Behavior.FontSize != 20 {
		t.Errorf("主题 behavior.font_size 应覆盖为 20, got %d", rv.Behavior.FontSize)
	}
	if rv.Behavior.ShowPageNumber != true {
		t.Errorf("未覆盖字段应保持基线 true, got %v", rv.Behavior.ShowPageNumber)
	}
}
