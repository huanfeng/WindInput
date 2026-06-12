package coordinator

import (
	"testing"
	"time"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/pkg/config"
)

// newSmartSymbolCoordinator 构造一个开启智能符号模式、处于中文标点模式的最小 Coordinator。
func newSmartSymbolCoordinator(t *testing.T) *testCoordinator {
	t.Helper()
	cfg := &config.Config{}
	cfg.Input.SmartSymbolMode = true
	cfg.Input.SmartSymbolTimeoutMs = 500
	cfg.Input.SmartSymbolChars = "。，？！：；"
	c := newTestCoordinator(t, withConfig(cfg))
	c.chinesePunctuation = true // isEffectiveChinesePunct 需要中文标点开启
	return c
}

// TestSmartSymbol_ArmThenTrigger 覆盖核心路径：press1 武装、press2 触发替换。
func TestSmartSymbol_ArmThenTrigger(t *testing.T) {
	c := newSmartSymbolCoordinator(t)

	// press1: 输入 。（prevChar=0，顶字/首次提交场景），应仅武装、不返回响应。
	if res := c.trySmartSymbolReplace('.', false, 0); res != nil {
		t.Fatalf("press1 应返回 nil，实际 %+v", res)
	}
	if !c.smartSymbolArmed || c.smartSymbolStr != "。" {
		t.Fatalf("press1 后应武装为 。，实际 armed=%v str=%q", c.smartSymbolArmed, c.smartSymbolStr)
	}

	// press2: 同键 . 且 prevChar=。、时限内，应返回 ReplaceBackward{1, "."} 并解除武装。
	res := c.trySmartSymbolReplace('.', false, '。')
	if res == nil {
		t.Fatal("press2 应触发替换，实际 nil")
	}
	if res.Type != bridge.ResponseTypeReplaceBackward || res.ReplaceCount != 1 || res.Text != "." {
		t.Fatalf("press2 替换响应不符：type=%v count=%d text=%q", res.Type, res.ReplaceCount, res.Text)
	}
	if c.smartSymbolArmed {
		t.Fatal("触发后应解除武装")
	}
}

// TestSmartSymbol_ShiftedSymbol 覆盖 Shift 类标点（？），逻辑层与无 Shift 一致。
func TestSmartSymbol_ShiftedSymbol(t *testing.T) {
	c := newSmartSymbolCoordinator(t)
	if res := c.trySmartSymbolReplace('?', false, 0); res != nil {
		t.Fatalf("press1 应 nil，实际 %+v", res)
	}
	res := c.trySmartSymbolReplace('?', false, '？')
	if res == nil || res.Text != "?" {
		t.Fatalf("？？应替换为单个英文 ?，实际 %+v", res)
	}
}

// TestSmartSymbol_Timeout 超时后不触发，且以本次重新武装。
func TestSmartSymbol_Timeout(t *testing.T) {
	c := newSmartSymbolCoordinator(t)
	c.trySmartSymbolReplace('.', false, 0)         // 武装
	c.smartSymbolAt = time.Now().Add(-time.Second) // 模拟超时（>500ms）
	if res := c.trySmartSymbolReplace('.', false, '。'); res != nil {
		t.Fatalf("超时不应触发，实际 %+v", res)
	}
	if !c.smartSymbolArmed {
		t.Fatal("超时后应以本次重新武装")
	}
}

// TestSmartSymbol_DifferentSymbol 不同符号不触发（同符号判定）。
func TestSmartSymbol_DifferentSymbol(t *testing.T) {
	c := newSmartSymbolCoordinator(t)
	c.trySmartSymbolReplace('.', false, 0) // 武装 。
	// 输入 ，（prevChar 仍为 。），cn=，≠武装的 。，不触发，并以 ， 重新武装。
	if res := c.trySmartSymbolReplace(',', false, '。'); res != nil {
		t.Fatalf("不同符号不应触发，实际 %+v", res)
	}
	if c.smartSymbolStr != "，" {
		t.Fatalf("应以 ， 重新武装，实际 %q", c.smartSymbolStr)
	}
}

// TestSmartSymbol_PrevCharMismatch 光标前字符校验不过则不触发（稳健性核心）。
func TestSmartSymbol_PrevCharMismatch(t *testing.T) {
	c := newSmartSymbolCoordinator(t)
	c.trySmartSymbolReplace('.', false, 0) // 武装 。
	// prevChar='x'（用户挪了光标 / 应用插了别的字）→ 不触发。
	if res := c.trySmartSymbolReplace('.', false, 'x'); res != nil {
		t.Fatalf("PrevChar 不匹配不应触发，实际 %+v", res)
	}
}

// TestSmartSymbol_PrevCharUnavailable prevChar=0（不可用）时不触发，安全退化。
func TestSmartSymbol_PrevCharUnavailable(t *testing.T) {
	c := newSmartSymbolCoordinator(t)
	c.trySmartSymbolReplace('.', false, 0) // 武装 。
	// 第二次 prevChar 仍为 0（C++ 未提供光标前字符）→ 不触发。
	if res := c.trySmartSymbolReplace('.', false, 0); res != nil {
		t.Fatalf("PrevChar 不可用不应触发，实际 %+v", res)
	}
}

// TestSmartSymbol_ModeOff 关闭开关时永不武装、永不触发。
func TestSmartSymbol_ModeOff(t *testing.T) {
	c := newSmartSymbolCoordinator(t)
	c.config.Input.SmartSymbolMode = false
	if res := c.trySmartSymbolReplace('.', false, 0); res != nil {
		t.Fatalf("关闭时应 nil，实际 %+v", res)
	}
	if c.smartSymbolArmed {
		t.Fatal("关闭时不应武装")
	}
}

// TestSmartSymbol_EnglishPunctMode 英文标点模式下既不触发也不武装（触发态也校验模式）。
func TestSmartSymbol_EnglishPunctMode(t *testing.T) {
	c := newSmartSymbolCoordinator(t)
	c.smartSymbolArmed = true
	c.smartSymbolKey = '.'
	c.smartSymbolStr = "。"
	c.smartSymbolAt = time.Now()
	c.chinesePunctuation = false // 切到英文标点
	if res := c.trySmartSymbolReplace('.', false, '。'); res != nil {
		t.Fatalf("英文标点模式不应触发，实际 %+v", res)
	}
	if c.smartSymbolArmed {
		t.Fatal("英文标点模式应解除武装")
	}
}

// TestSmartSymbol_Quote 引号：press1 产 “、press2 产 ”，按 press1 的 “ 匹配触发，替换为英文 "。
func TestSmartSymbol_Quote(t *testing.T) {
	c := newSmartSymbolCoordinator(t)
	c.config.Input.SmartSymbolChars = "。，“”" // 参与集含中文双引号

	// press1: " → “（默认 doubleQuoteLeft=true），武装
	if res := c.trySmartSymbolReplace('"', false, 0); res != nil {
		t.Fatalf("\" press1 应 nil，实际 %+v", res)
	}
	if c.smartSymbolStr != "“" {
		t.Fatalf("应武装为 “，实际 %q", c.smartSymbolStr)
	}
	// 模拟 press1 的 convertPunct 翻转引号状态（左→右）
	c.punctConverter.ToChinesePunct('"')

	// press2: " 且 prevChar=“，应删 1 替换为英文直引号 "
	res := c.trySmartSymbolReplace('"', false, '“')
	if res == nil || res.ReplaceCount != 1 || res.Text != "\"" {
		t.Fatalf("“ 双击应替换为 \"，实际 %+v", res)
	}
}

// TestSmartSymbol_NonParticipatingChar 不在参与集合内的标点不参与（如括号 ()→（）)。
func TestSmartSymbol_NonParticipatingChar(t *testing.T) {
	c := newSmartSymbolCoordinator(t)
	c.smartSymbolArmed = true
	c.smartSymbolStr = "。"
	// '(' → （，不在默认参与集 → 解除武装、返回 nil。
	if res := c.trySmartSymbolReplace('(', false, 0); res != nil {
		t.Fatalf("非参与标点不应触发，实际 %+v", res)
	}
	if c.smartSymbolArmed {
		t.Fatal("非参与标点应解除武装")
	}
}

// TestSmartSymbol_TriplePress 三连击 。。。：第三次因 prevChar 为英文 . 不再触发，正常落中文。
func TestSmartSymbol_TriplePress(t *testing.T) {
	c := newSmartSymbolCoordinator(t)
	c.trySmartSymbolReplace('.', false, 0)         // press1：武装
	r2 := c.trySmartSymbolReplace('.', false, '。') // press2：触发替换为 .
	if r2 == nil {
		t.Fatal("press2 应触发")
	}
	// press3：此时光标前是英文 .（非武装的 。），且已解除武装 → 不触发，重新武装。
	r3 := c.trySmartSymbolReplace('.', false, '.')
	if r3 != nil {
		t.Fatalf("press3 不应触发，实际 %+v", r3)
	}
	if !c.smartSymbolArmed {
		t.Fatal("press3 后应重新武装")
	}
}

// TestSmartSymbol_MultiChar 多字符标点（省略号 ……，由 ^ 键产生）：删 2 个 rune，替换为 ^。
func TestSmartSymbol_MultiChar(t *testing.T) {
	c := newSmartSymbolCoordinator(t)
	c.config.Input.SmartSymbolChars = "。，……" // 参与集含省略号
	if res := c.trySmartSymbolReplace('^', false, 0); res != nil {
		t.Fatalf("press1 应 nil，实际 %+v", res)
	}
	if c.smartSymbolStr != "……" {
		t.Fatalf("应武装为 ……，实际 %q", c.smartSymbolStr)
	}
	// press2: ^ 且 prevChar=…（…… 末位 rune），应删 2 个 rune 并插入英文原字符 ^。
	res := c.trySmartSymbolReplace('^', false, '…')
	if res == nil || res.Type != bridge.ResponseTypeReplaceBackward || res.ReplaceCount != 2 || res.Text != "^" {
		t.Fatalf("……应删2插^，实际 %+v", res)
	}
}

// TestSmartSymbol_FullWidth 全角模式下替换为全角英文（。。 → ．）。
func TestSmartSymbol_FullWidth(t *testing.T) {
	c := newSmartSymbolCoordinator(t)
	c.fullWidth = true
	if res := c.trySmartSymbolReplace('.', false, 0); res != nil {
		t.Fatalf("press1 应 nil，实际 %+v", res)
	}
	// 全角下 。 仍是 。（ToFullWidth 对 CJK no-op），prevChar=。
	res := c.trySmartSymbolReplace('.', false, '。')
	if res == nil || res.ReplaceCount != 1 || res.Text != "．" {
		t.Fatalf("全角下应替换为全角英文 ．，实际 %+v", res)
	}
}

// TestSmartSymbol_CustomMapping 自定义映射的键也参与，且按记录的原始键各自还原：
// / 自定义为 、，\ 默认为 、，双击 // → /，\\ → \。
func TestSmartSymbol_CustomMapping(t *testing.T) {
	c := newSmartSymbolCoordinator(t)
	c.config.Input.SmartSymbolChars = "。，、" // 参与集含顿号
	c.config.Input.PunctCustom.Enabled = true
	c.config.Input.PunctCustom.Mappings = map[string][]string{
		"/": {"、", "", ""}, // 把 / 自定义为中文半角 、
	}

	// / → 、（自定义），双击还原为 /
	if res := c.trySmartSymbolReplace('/', false, 0); res != nil {
		t.Fatalf("/ press1 应 nil，实际 %+v", res)
	}
	if c.smartSymbolStr != "、" {
		t.Fatalf("/ 应武装为 、，实际 %q", c.smartSymbolStr)
	}
	res := c.trySmartSymbolReplace('/', false, '、')
	if res == nil || res.Text != "/" {
		t.Fatalf("//（自定义→、）应还原为 /，实际 %+v", res)
	}

	// \ → 、（默认映射），双击还原为 \（验证原始键消歧）
	if res := c.trySmartSymbolReplace('\\', false, 0); res != nil {
		t.Fatalf("\\ press1 应 nil，实际 %+v", res)
	}
	res = c.trySmartSymbolReplace('\\', false, '、')
	if res == nil || res.Text != "\\" {
		t.Fatalf("\\\\（默认→、）应还原为 \\，实际 %+v", res)
	}
}

// TestSmartSymbol_DisarmClearsState disarmSmartSymbol 后下一次同键不触发。
func TestSmartSymbol_DisarmClearsState(t *testing.T) {
	c := newSmartSymbolCoordinator(t)
	c.trySmartSymbolReplace('.', false, 0) // 武装
	c.disarmSmartSymbol()                  // 模拟焦点丢失
	if res := c.trySmartSymbolReplace('.', false, '。'); res != nil {
		t.Fatalf("disarm 后不应触发，实际 %+v", res)
	}
}
