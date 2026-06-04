package theme

import (
	"encoding/base64"
	"os"
	"path/filepath"
	"testing"
)

const testSVGCircle = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><circle cx="50" cy="50" r="45" fill="#000000"/></svg>`

func TestIsSVGRef(t *testing.T) {
	cases := map[string]bool{
		"icon.svg":                       true,
		"ICON.SVG":                       true,
		"data:image/svg+xml,<svg/>":      true,
		"data:image/svg+xml;base64,abcd": true,
		"icon.png":                       false,
		"data:image/png;base64,abcd":     false,
		"":                               false,
	}
	for ref, want := range cases {
		if got := IsSVGRef(ref); got != want {
			t.Errorf("IsSVGRef(%q)=%v, 期望 %v", ref, got, want)
		}
	}
}

// assertCircleRaster 校验栅格化结果：尺寸正确、圆心不透明、角落透明。
func assertCircleRaster(t *testing.T, ref string) {
	t.Helper()
	img, err := RasterizeSVG(ref, 32, 32)
	if err != nil {
		t.Fatalf("RasterizeSVG(%q): %v", ref, err)
	}
	if img.Bounds().Dx() != 32 || img.Bounds().Dy() != 32 {
		t.Fatalf("尺寸应 32x32, got %v", img.Bounds())
	}
	if _, _, _, a := img.At(16, 16).RGBA(); a == 0 {
		t.Error("圆心应非透明")
	}
	if _, _, _, a := img.At(0, 0).RGBA(); a != 0 {
		t.Error("角落应透明（圆外）")
	}
}

func TestRasterizeSVG_File(t *testing.T) {
	p := filepath.Join(t.TempDir(), "circle.svg")
	if err := os.WriteFile(p, []byte(testSVGCircle), 0o644); err != nil {
		t.Fatal(err)
	}
	assertCircleRaster(t, p)
}

func TestRasterizeSVG_DataURIBase64(t *testing.T) {
	uri := "data:image/svg+xml;base64," + base64.StdEncoding.EncodeToString([]byte(testSVGCircle))
	assertCircleRaster(t, uri)
}

func TestRasterizeSVG_BadSize(t *testing.T) {
	if _, err := RasterizeSVG("x.svg", 0, 10); err == nil {
		t.Error("尺寸<=0 应报错")
	}
}
