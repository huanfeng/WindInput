package ui

import (
	"os"
	"regexp"
	"strings"
	"testing"
)

// TestNoDoubleScale 防回归：禁止 sc(N*scale) 写法。
// 构建器里 sc(v)=int(v*scale+0.5) 自身已乘一次 scale，sc(N*scale)=N*scale² 会在高 DPI 下失真。
// 需要随 DPI 缩放的固定像素常量应写 sc(N)；不缩放的内禀几何用裸值或 px 单位。
func TestNoDoubleScale(t *testing.T) {
	pat := regexp.MustCompile(`sc\([0-9.]+\s*\*\s*scale\)`)
	entries, err := os.ReadDir(".")
	if err != nil {
		t.Fatal(err)
	}
	for _, e := range entries {
		name := e.Name()
		if e.IsDir() || !strings.HasSuffix(name, ".go") || strings.HasSuffix(name, "_test.go") {
			continue
		}
		data, err := os.ReadFile(name)
		if err != nil {
			t.Fatal(err)
		}
		for i, line := range strings.Split(string(data), "\n") {
			if pat.MatchString(line) {
				t.Errorf("%s:%d 双重缩放 sc(N*scale)，应改为 sc(N): %s", name, i+1, strings.TrimSpace(line))
			}
		}
	}
}
