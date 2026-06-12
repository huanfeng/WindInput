package coordinator

// handle_punct_custom_test.go — 自定义符号映射功能测试
//
// Feature 1: convertPunct 四状态均可配置（含英文半角 colIdx=3）
// Feature 2: handleSpace 支持空格映射（通过 PunctCustom.Mappings[" "]）
//
// 内部存储顺序：[中文半角=0, 英文全角=1, 中文全角=2, 英文半角=3]
// 默认状态：chineseMode=true, chinesePunctuation=false, fullWidth=false → EN半角 colIdx=3

import (
	"testing"

	"github.com/huanfeng/wind_input/internal/bridge"
)

// ─── Feature 1: convertPunct 四状态 ─────────────────────────────────────────

// TestPunctCustom_EnHalf_BasicMapping 验证英文半角（默认状态）的自定义映射生效.
// 默认 testCoordinator：chineseMode=true, chinesePunctuation=false, fullWidth=false → colIdx=3.
func TestPunctCustom_EnHalf_BasicMapping(t *testing.T) {
	c := newTestCoordinator(t, withPunctCustom(map[string][]string{
		";": {"", "", "", "；"}, // 英半 (internal[3]) = 全角分号
	}))
	got := c.convertPunct(';', false, 0)
	if got != "；" {
		t.Fatalf("英半自定义映射：期望 '；'，实际 %q", got)
	}
}

// TestPunctCustom_EnHalf_Fallback 未配置英半时回退到默认转换逻辑.
func TestPunctCustom_EnHalf_Fallback(t *testing.T) {
	c := newTestCoordinator(t, withPunctCustom(map[string][]string{
		";": {"；", "", "", ""}, // 仅配置中半，英半留空
	}))
	// 英半无配置 → LookupCustom 返回 false → 走默认路径（英文半角直通，返回空串）
	got := c.convertPunct(';', false, 0)
	if got == "；" {
		t.Fatal("英半未配置时不应返回中文符号")
	}
}

// TestPunctCustom_OldConfig_Compat 旧 3-列配置（len=3）不应影响 colIdx=3（越界保护）.
func TestPunctCustom_OldConfig_Compat(t *testing.T) {
	c := newTestCoordinator(t, withPunctCustom(map[string][]string{
		";": {"；", "", ""}, // 仅 3 项，无 internal[3]
	}))
	// colIdx=3 → len(vals)=3 → bounds check → LookupCustom returns false → 默认路径
	got := c.convertPunct(';', false, 0)
	if got == "；" {
		t.Fatal("旧 3-列配置不应干扰英半路径")
	}
}

// TestPunctCustom_FourStates_Independent 验证四个状态互相独立，配置不串扰.
func TestPunctCustom_FourStates_Independent(t *testing.T) {
	c := newTestCoordinator(t, withPunctCustom(map[string][]string{
		";": {"A", "B", "C", "D"}, // 中半=A, 英全=B, 中全=C, 英半=D
	}))

	tests := []struct {
		name         string
		chineseMode  bool
		chinesePunct bool
		fullWidth    bool
		want         string
	}{
		{"英半(colIdx=3)", true, false, false, "D"},
		{"英全(colIdx=1)", false, false, true, "B"},
		{"中半(colIdx=0)", true, true, false, "A"},
		{"中全(colIdx=2)", true, true, true, "C"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			c.chineseMode = tt.chineseMode
			c.chinesePunctuation = tt.chinesePunct
			c.fullWidth = tt.fullWidth
			got := c.convertPunct(';', false, 0)
			if got != tt.want {
				t.Fatalf("期望 %q，实际 %q", tt.want, got)
			}
		})
	}
}

// TestPunctCustom_EnHalf_QuoteSwap 英半引号互换：双引→单引、单引→双引.
func TestPunctCustom_EnHalf_QuoteSwap(t *testing.T) {
	// 双引号 key="1 (左双引号)、"2 (右双引号)；单引 '1/'2 同理
	// 英半模式下把双引映射到单引、单引映射到双引
	c := newTestCoordinator(t, withPunctCustom(map[string][]string{
		`"1`: {"", "", "", "'"}, // 左双引号 → 英半单引号
		`"2`: {"", "", "", "'"}, // 右双引号 → 英半单引号
		`'1`: {"", "", "", `"`}, // 左单引号 → 英半双引号
		`'2`: {"", "", "", `"`}, // 右单引号 → 英半双引号
	}))
	// 英半模式（默认）
	c.chinesePunctuation = false

	// 初始状态 doubleQuoteLeft=true → 第一次 " 产生左双 "1
	got := c.convertPunct('"', false, 0)
	if got != "'" {
		t.Fatalf("英半双引号期望映射到单引号 '，实际 %q", got)
	}
}

// ─── Feature 2: handleSpace 空格映射 ────────────────────────────────────────

// TestHandleSpace_CustomSpace_ChineseHalf 中文半角模式下空格映射到全角空格.
// chineseMode=true, fullWidth=false → colIdx=0 (中文半角)
func TestHandleSpace_CustomSpace_ChineseHalf(t *testing.T) {
	c := newTestCoordinator(t, withPunctCustom(map[string][]string{
		" ": {"　", "", "", ""}, // 中半(internal[0]) = 全角空格
	}))
	// 默认：chineseMode=true, fullWidth=false, 无候选, 无 inputBuffer → idle branch
	res := c.handleSpace()
	if res == nil || res.Type != bridge.ResponseTypeInsertText || res.Text != "　" {
		t.Fatalf("中文半角空格期望输出全角空格，实际 %+v", res)
	}
}

// TestHandleSpace_CustomSpace_EnglishHalf 英文半角模式下空格映射到全角空格.
// chineseMode=false, fullWidth=false → colIdx=3 (英文半角)
func TestHandleSpace_CustomSpace_EnglishHalf(t *testing.T) {
	c := newTestCoordinator(t,
		withChineseMode(false),
		withPunctCustom(map[string][]string{
			" ": {"", "", "", "　"}, // 英半(internal[3]) = 全角空格
		}),
	)
	res := c.handleSpace()
	if res == nil || res.Type != bridge.ResponseTypeInsertText || res.Text != "　" {
		t.Fatalf("英文半角空格期望输出全角空格，实际 %+v", res)
	}
}

// TestHandleSpace_CustomSpace_ExplicitHalf 中文半角显式映射到半角空格时应存储且正确返回.
// 这验证了 Bug 2 的修复：英半/中半 default="" 使 " " 可被存储为有效覆盖.
func TestHandleSpace_CustomSpace_ExplicitHalf(t *testing.T) {
	c := newTestCoordinator(t, withPunctCustom(map[string][]string{
		" ": {" ", "", "", ""}, // 中半(internal[0]) = 显式半角空格
	}))
	res := c.handleSpace()
	if res == nil || res.Type != bridge.ResponseTypeInsertText || res.Text != " " {
		t.Fatalf("显式半角空格映射应返回 InsertText ' '，实际 %+v", res)
	}
}

// TestHandleSpace_NoSpaceMapping_FullWidthFallback 无空格映射时 fullWidth=true 回退全角空格.
func TestHandleSpace_NoSpaceMapping_FullWidthFallback(t *testing.T) {
	c := newTestCoordinator(t, withPunctCustom(map[string][]string{
		";": {"；", "", "", ""}, // 有其他映射但无空格行
	}))
	c.fullWidth = true
	res := c.handleSpace()
	if res == nil || res.Type != bridge.ResponseTypeInsertText || res.Text != string(rune(0x3000)) {
		t.Fatalf("无空格映射+全角模式应回退全角空格，实际 %+v", res)
	}
}

// TestHandleSpace_CustomDisabled_FullWidthFallback 关闭自定义映射时全角模式正常回退.
func TestHandleSpace_CustomDisabled_FullWidthFallback(t *testing.T) {
	c := newTestCoordinator(t)
	// 不启用 PunctCustom，只开全角
	c.fullWidth = true
	res := c.handleSpace()
	if res == nil || res.Type != bridge.ResponseTypeInsertText || res.Text != string(rune(0x3000)) {
		t.Fatalf("自定义未启用+全角模式应输出全角空格，实际 %+v", res)
	}
}

// TestHandleSpace_IdleNoConfig 无配置无全角时 handleSpace 应透传（返回 nil）.
func TestHandleSpace_IdleNoConfig(t *testing.T) {
	c := newTestCoordinator(t)
	res := c.handleSpace()
	if res != nil {
		t.Fatalf("无配置无全角时期望 nil，实际 %+v", res)
	}
}
