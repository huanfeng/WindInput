//go:build windows

package ui

import "testing"

// TestNewSharedDrawContext_ZeroSize 守护回归（2026-06-04）：尺寸为 0/负（异常主题致几何塌缩）
// 时应 clamp 到 1×1 不 panic——否则 gg.NewPixmapFromBuffer panic「width and height must be > 0」，
// 候选窗/独立窗口崩溃消失。配合 manager 层「非 v3 主题回退 default」为纵深防御。
func TestNewSharedDrawContext_ZeroSize(t *testing.T) {
	defer func() {
		if r := recover(); r != nil {
			t.Fatalf("0 尺寸不应 panic: %v", r)
		}
	}()
	dc, img := newSharedDrawContext(0, 0)
	if dc == nil || img == nil {
		t.Fatal("应返回有效 1×1 上下文")
	}
	if b := img.Bounds(); b.Dx() < 1 || b.Dy() < 1 {
		t.Errorf("clamp 后应至少 1×1, got %dx%d", b.Dx(), b.Dy())
	}
}
