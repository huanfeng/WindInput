// routing_basic_test.go — HandleKeyEvent 最浅层路由的烟雾测试.
//
// 覆盖"不触达 engineMgr / uiManager"的早期直通分支, 主要目的是验证
// testhelper_test.go 提供的脚手架本身可用. 真正的输入路径用例（候选生成、
// 临时拼音、z 混合决策等）依赖 engine fixture, 等后续提交补.
package coordinator

import (
	"testing"

	"github.com/huanfeng/wind_input/internal/bridge"
)

// 英文模式下字母键应直通给宿主, HandleKeyEvent 返回 nil, 不进入 IME 流水线.
func TestRouting_EnglishMode_LetterPassThrough(t *testing.T) {
	h := newTestCoordinator(t, withChineseMode(false))
	r := h.pressKey("a")

	if r != nil {
		t.Fatalf("expected nil pass-through for English-mode letter, got %+v", r)
	}
	if h.inputBuffer != "" {
		t.Errorf("inputBuffer should remain empty, got %q", h.inputBuffer)
	}
}

// 英文模式 + 全角开启时, 字母应被转为全角并 InsertText.
func TestRouting_EnglishMode_FullWidthLetter(t *testing.T) {
	h := newTestCoordinator(t, withChineseMode(false))
	h.fullWidth = true

	r := h.pressKey("a")
	if r == nil {
		t.Fatal("expected InsertText for full-width English-mode letter, got nil")
	}
	if r.Type != bridge.ResponseTypeInsertText {
		t.Errorf("result type = %v, want InsertText", r.Type)
	}
	// "ａ" = 全角 a (U+FF41)
	if r.Text != "ａ" {
		t.Errorf("Text = %q, want %q", r.Text, "ａ")
	}
}

// Ctrl+字母组合在没有 pending input 时应直通（返回 nil）, 让宿主消费快捷键.
func TestRouting_CtrlCombo_PassThrough(t *testing.T) {
	h := newTestCoordinator(t)
	r := h.HandleKeyEvent(bridge.KeyEventData{
		Key:       "c",
		KeyCode:   int('C'),
		Modifiers: ModCtrl,
	})
	if r != nil {
		t.Errorf("expected nil pass-through for Ctrl+C without pending input, got %+v", r)
	}
}
