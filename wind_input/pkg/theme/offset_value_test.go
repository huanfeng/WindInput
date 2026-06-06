package theme

import (
	"encoding/json"
	"image"
	"testing"

	"gopkg.in/yaml.v3"
)

// TestOffsetValue_ParseYAML 验证 offset 分量的 YAML 解析：裸数字→dp、"N%"→百分比、"Ndp"→dp。
func TestOffsetValue_ParseYAML(t *testing.T) {
	var p ViewImagePoint
	if err := yaml.Unmarshal([]byte("x: -10%\ny: -8\n"), &p); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if !p.X.IsPct || p.X.Pct != -10 {
		t.Errorf("x 应解析为百分比 -10，got %+v", p.X)
	}
	if p.Y.IsPct || p.Y.DP != -8 {
		t.Errorf("y 应解析为 dp -8，got %+v", p.Y)
	}

	var p2 ViewImagePoint
	if err := yaml.Unmarshal([]byte("x: 12dp\n"), &p2); err != nil {
		t.Fatalf("unmarshal dp 后缀: %v", err)
	}
	if p2.X.IsPct || p2.X.DP != 12 {
		t.Errorf("x \"12dp\" 应为 dp 12，got %+v", p2.X)
	}
}

// TestOffsetValue_RoundTrip 验证 YAML/JSON marshal round-trip：dp→裸数字、百分比→"N%"。
func TestOffsetValue_RoundTrip(t *testing.T) {
	cases := []OffsetValue{OffsetDp(-8), OffsetPct(-10), OffsetDp(0), OffsetPct(33.5)}
	for _, c := range cases {
		// YAML
		out, err := yaml.Marshal(c)
		if err != nil {
			t.Fatalf("yaml marshal %+v: %v", c, err)
		}
		var back OffsetValue
		if err := yaml.Unmarshal(out, &back); err != nil {
			t.Fatalf("yaml unmarshal %q: %v", out, err)
		}
		if back != c {
			t.Errorf("yaml round-trip: %+v → %q → %+v", c, out, back)
		}
		// JSON
		jb, err := json.Marshal(c)
		if err != nil {
			t.Fatalf("json marshal %+v: %v", c, err)
		}
		var jback OffsetValue
		if err := json.Unmarshal(jb, &jback); err != nil {
			t.Fatalf("json unmarshal %q: %v", jb, err)
		}
		if jback != c {
			t.Errorf("json round-trip: %+v → %q → %+v", c, jb, jback)
		}
	}
}

// TestOffsetValue_Split 验证拆分到 (dp, pct)。
func TestOffsetValue_Split(t *testing.T) {
	if dp, pct := OffsetDp(7).Split(); dp != 7 || pct != 0 {
		t.Errorf("dp 7 Split 应 (7,0)，got (%d,%v)", dp, pct)
	}
	if dp, pct := OffsetPct(20).Split(); dp != 0 || pct != 20 {
		t.Errorf("pct 20 Split 应 (0,20)，got (%d,%v)", dp, pct)
	}
}

// solidRGBA 造一张全不透明纯色图（预乘 alpha；纯色 alpha=255 时预乘=原值）。
func solidRGBA(w, h int, r, g, b uint8) *image.RGBA {
	img := image.NewRGBA(image.Rect(0, 0, w, h))
	for i := 0; i < len(img.Pix); i += 4 {
		img.Pix[i], img.Pix[i+1], img.Pix[i+2], img.Pix[i+3] = r, g, b, 255
	}
	return img
}

// TestDrawBackgroundClipped_HardClip 验证矩形硬裁：目标 rect 超出 clip 时，clip 外像素不被绘制。
func TestDrawBackgroundClipped_HardClip(t *testing.T) {
	dst := solidRGBA(10, 10, 0, 0, 0) // 黑底
	src := solidRGBA(8, 8, 255, 0, 0) // 红图
	// 目标 rect 覆盖 (2,2)-(10,10)，但只允许画进 clip=(2,2)-(6,6)。
	target := image.Rect(2, 2, 10, 10)
	clip := image.Rect(2, 2, 6, 6)
	DrawBackgroundClipped(dst, target, src, "stretch", Padding{}, 1.0, 0, clip)

	at := func(x, y int) (uint8, uint8, uint8) {
		o := dst.PixOffset(x, y)
		return dst.Pix[o], dst.Pix[o+1], dst.Pix[o+2]
	}
	// clip 内 → 红
	if r, _, _ := at(3, 3); r != 255 {
		t.Errorf("clip 内 (3,3) 应为红，got r=%d", r)
	}
	// clip 外但 target 内 → 仍黑（被硬裁）
	if r, _, _ := at(7, 7); r != 0 {
		t.Errorf("clip 外 (7,7) 应保持黑（硬裁），got r=%d", r)
	}
	if r, _, _ := at(2, 7); r != 0 {
		t.Errorf("clip 外 (2,7) 应保持黑（硬裁），got r=%d", r)
	}
}

// TestDrawBackground_NoClipRegression 验证全覆盖路径（DrawBackground=clip 自身）逐像素铺满，零回归。
func TestDrawBackground_NoClipRegression(t *testing.T) {
	dst := solidRGBA(6, 6, 0, 0, 0)
	src := solidRGBA(6, 6, 0, 255, 0)
	DrawBackground(dst, image.Rect(0, 0, 6, 6), src, "stretch", Padding{}, 1.0, 0)
	o := dst.PixOffset(5, 5)
	if dst.Pix[o+1] != 255 {
		t.Errorf("全覆盖应铺满至 (5,5)，got g=%d", dst.Pix[o+1])
	}
}
