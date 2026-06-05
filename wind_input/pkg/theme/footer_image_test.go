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
