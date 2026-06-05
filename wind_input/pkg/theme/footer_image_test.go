package theme

import (
	"image/color"
	"os"
	"path/filepath"
	"runtime"
	"testing"
)

// sourceThemesDir 返回仓库内 themes/ 目录的绝对路径（直接使用源码目录，无需构建产物）。
func sourceThemesDir(t *testing.T) string {
	t.Helper()
	_, file, _, _ := runtime.Caller(0)
	// file = .../wind_input/pkg/theme/footer_image_test.go → 上溯 2 层到 wind_input/，再进 themes/
	dir := filepath.Join(filepath.Dir(file), "..", "..", "themes")
	abs, err := filepath.Abs(dir)
	if err != nil {
		t.Fatal(err)
	}
	if _, statErr := os.Stat(abs); os.IsNotExist(statErr) {
		t.Skipf("跳过测试：themes/ 目录不存在：%s", abs)
	}
	return abs
}

// TestFooterImagesInheritedFromBase 守护回归：继承自 _base 的翻页箭头图（prev_image/next_image）
// 在 default/msime 等子主题中必须解析为可访问的绝对路径——禁止路径因 selfDir 错位而断裂。
func TestFooterImagesInheritedFromBase(t *testing.T) {
	dir := sourceThemesDir(t)
	m := &Manager{themeDirs: []string{dir}}

	for _, id := range []string{"default", "msime"} {
		t.Run(id, func(t *testing.T) {
			if err := m.LoadTheme(id); err != nil {
				t.Fatalf("LoadTheme %s: %v", id, err)
			}
			r := m.GetResolvedV3()
			if r == nil {
				t.Fatal("resolved nil")
			}
			fb := r.Views.FooterBar
			if fb.PrevImage == nil {
				t.Fatalf("theme=%s: FooterBar.PrevImage 为 nil，_base 继承路径断裂", id)
			}
			if fb.NextImage == nil {
				t.Fatalf("theme=%s: FooterBar.NextImage 为 nil，_base 继承路径断裂", id)
			}
			if !filepath.IsAbs(fb.PrevImage.Ref) {
				t.Errorf("theme=%s: PrevImage.Ref 应为绝对路径，got %q", id, fb.PrevImage.Ref)
			}
			if _, err := os.Stat(fb.PrevImage.Ref); err != nil {
				t.Errorf("theme=%s: PrevImage.Ref 文件不存在：%q", id, fb.PrevImage.Ref)
			}
			if _, err := os.Stat(fb.NextImage.Ref); err != nil {
				t.Errorf("theme=%s: NextImage.Ref 文件不存在：%q", id, fb.NextImage.Ref)
			}
		})
	}
}

// TestMergeViewNode_CharClearsInheritedImage 守护回归：子主题显式设置 PrevChar/NextChar 时
// 必须清除从 base 继承来的 PrevImage/NextImage，否则图片优先级会永远压制字符配置。
func TestMergeViewNode_CharClearsInheritedImage(t *testing.T) {
	base := ViewNode{
		PrevImage: &ViewImage{Ref: "chevron_prev.svg"},
		NextImage: &ViewImage{Ref: "chevron_next.svg"},
	}
	customPrev := "◀"
	customNext := "▶"
	ov := ViewNode{
		PrevChar: &customPrev,
		NextChar: &customNext,
	}
	out := mergeViewNode(base, ov)
	if out.PrevImage != nil {
		t.Errorf("子主题配了 PrevChar，应清除继承的 PrevImage，got %+v", out.PrevImage)
	}
	if out.NextImage != nil {
		t.Errorf("子主题配了 NextChar，应清除继承的 NextImage，got %+v", out.NextImage)
	}
	if out.PrevChar == nil || *out.PrevChar != customPrev {
		t.Errorf("PrevChar 应为 %q，got %v", customPrev, out.PrevChar)
	}
	if out.NextChar == nil || *out.NextChar != customNext {
		t.Errorf("NextChar 应为 %q，got %v", customNext, out.NextChar)
	}
}

// TestMergeViewNode_ImageClearsInheritedChar 守护回归：子主题显式设置 PrevImage/NextImage 时
// 应清除 base 可能存在的字符（保持互斥语义）。
func TestMergeViewNode_ImageClearsInheritedChar(t *testing.T) {
	prev := "◀"
	next := "▶"
	base := ViewNode{PrevChar: &prev, NextChar: &next}
	ov := ViewNode{
		PrevImage: &ViewImage{Ref: "arrow_left.svg"},
		NextImage: &ViewImage{Ref: "arrow_right.svg"},
	}
	out := mergeViewNode(base, ov)
	if out.PrevChar != nil {
		t.Errorf("子主题配了 PrevImage，应清除继承的 PrevChar，got %v", out.PrevChar)
	}
	if out.NextChar != nil {
		t.Errorf("子主题配了 NextImage，应清除继承的 NextChar，got %v", out.NextChar)
	}
	if out.PrevImage == nil || out.PrevImage.Ref != "arrow_left.svg" {
		t.Errorf("PrevImage 应保留，got %v", out.PrevImage)
	}
}

// TestMergeViewNode_FooterImages 守护回归：footer_bar 的 prev_image/next_image 必须能通过
// base 单链继承的 mergeViewNode 保留（曾漏合并这两个字段，导致薄主题配的翻页箭头图被静默丢弃）。
func TestMergeViewNode_FooterImages(t *testing.T) {
	base := ViewNode{Color: NewLightDark("${footer}")}
	ov := ViewNode{
		PrevImage: &ViewImage{Ref: "arrow_left", Tint: NewLightDark("#112233")},
		NextImage: &ViewImage{Ref: "arrow_right"},
	}
	out := mergeViewNode(base, ov)
	if out.PrevImage == nil || out.PrevImage.Ref != "arrow_left" {
		t.Fatalf("PrevImage 应通过 merge 保留, got %v", out.PrevImage)
	}
	if out.NextImage == nil || out.NextImage.Ref != "arrow_right" {
		t.Fatalf("NextImage 应通过 merge 保留, got %v", out.NextImage)
	}

	// resolve：tint token → TintColor。
	rv := resolveViewNode(out, func(c ColorRef) color.Color {
		if c.Select(false) == "#112233" {
			return color.RGBA{0x11, 0x22, 0x33, 0xff}
		}
		return nil
	}, nil, nil, nil)
	if rv.PrevImage == nil || rv.PrevImage.TintColor == nil {
		t.Fatal("resolve 后 PrevImage.TintColor 应非 nil")
	}
	if rv.NextImage == nil || rv.NextImage.TintColor != nil {
		t.Fatal("NextImage 未配 tint，TintColor 应为 nil")
	}
}
