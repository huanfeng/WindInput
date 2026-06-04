package theme

import (
	"image/color"
	"testing"
)

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
