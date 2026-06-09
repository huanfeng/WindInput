// handle_special_mode_test.go — 特殊模式状态机集成测试。
// 使用真实 specialModeRegistry + 真实码表（testdata/special_symbols.dict.yaml）。
package coordinator

import (
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/pkg/config"
)

// specialModeCfg 构造测试用的特殊模式配置（prefix_free，使用 grave 触发）。
func specialModeCfg() config.SpecialModeConfig {
	return config.SpecialModeConfig{
		ID:          "sym",
		Name:        "快符",
		TriggerKeys: []string{"grave"},
		Table:       "special_symbols.dict.yaml",
		AutoCommit:  config.SpecialAutoCommitPrefixFree,
	}
}

// newSpecialTestCoordinator 构造已注入 specialModeReg 的 testCoordinator。
func newSpecialTestCoordinator(t *testing.T) *testCoordinator {
	t.Helper()
	tc := newTestCoordinator(t)
	dir, _ := filepath.Abs("testdata")
	tc.specialModeReg = newSpecialModeRegistry(
		[]config.SpecialModeConfig{specialModeCfg()},
		[]string{dir},
		testSpecialLogger(),
	)
	return tc
}

// enterSpecialMode 触发 grave 键进入特殊模式，断言成功并返回结果。
func enterSpecialMode(t *testing.T, tc *testCoordinator) *bridge.KeyEventResult {
	t.Helper()
	res := tc.HandleKeyEvent(bridge.KeyEventData{
		Key:     "`",
		KeyCode: int(ipc.VK_OEM_3),
	})
	if !tc.specialMode {
		t.Fatal("enterSpecialMode: specialMode should be true after grave key")
	}
	return res
}

// TestSpecialMode_AutoCommit_ArrowExact 打 "arrow" → 唯一候选且无更长编码 → 自动上屏 "⇧"
func TestSpecialMode_AutoCommit_ArrowExact(t *testing.T) {
	tc := newSpecialTestCoordinator(t)
	enterSpecialMode(t, tc)

	// 逐字符输入 "arrow"
	for _, ch := range "arrow" {
		tc.pressKey(string(ch))
	}

	// 最后一个字母 w 应触发自动上屏
	// 由于 pressKey 在 for 循环中已调用，这里检查最终状态
	if tc.specialMode {
		t.Error("specialMode should be false after auto-commit")
	}
}

// TestSpecialMode_AutoCommit_ArrowFinalKey "arrow" 最后一字触发自动上屏，检查返回值
func TestSpecialMode_AutoCommit_ArrowFinalKey(t *testing.T) {
	tc := newSpecialTestCoordinator(t)
	enterSpecialMode(t, tc)

	// 输入前四个字母 "arro"
	for _, ch := range "arro" {
		tc.pressKey(string(ch))
	}
	// 此时仍在模式中
	if !tc.specialMode {
		t.Fatal("should still be in special mode after 'arro'")
	}

	// 输入最后一个字母 "w" → autoCommit
	res := tc.pressKey("w")
	if res == nil {
		t.Fatal("result should not be nil")
	}
	if res.Type != bridge.ResponseTypeInsertText {
		t.Fatalf("expected InsertText, got %q", res.Type)
	}
	if res.Text != "⇧" {
		t.Errorf("expected ⇧, got %q", res.Text)
	}
	if tc.specialMode {
		t.Error("specialMode should be false after auto-commit")
	}
}

// TestSpecialMode_PrefixHasLonger "ar" → HasLongerCode true → 不自动上屏，仍在模式
// Lookup("ar") 返回空（无精确匹配），但 HasLongerCode("ar") 为 true，
// 因此 decideSpecialAutoCommit(prefix_free, ...) 返回 false，模式保持激活。
func TestSpecialMode_PrefixHasLonger(t *testing.T) {
	tc := newSpecialTestCoordinator(t)
	enterSpecialMode(t, tc)

	tc.pressKey("a")
	tc.pressKey("r")

	if !tc.specialMode {
		t.Error("should still be in special mode after 'ar' (hasLonger=true, no auto-commit)")
	}
	// "ar" 无精确匹配，所以 candidates 为空，但模式仍激活（未自动上屏）
	if tc.specialBuffer != "ar" {
		t.Errorf("expected buffer 'ar', got %q", tc.specialBuffer)
	}
}

// TestSpecialMode_JT_TwoCandidates 打 "jt" → 2 候选，不自动上屏；数字 '1' 选 "→" 退出
func TestSpecialMode_JT_TwoCandidates(t *testing.T) {
	tc := newSpecialTestCoordinator(t)
	enterSpecialMode(t, tc)

	tc.pressKey("j")
	res := tc.pressKey("t")
	_ = res

	if !tc.specialMode {
		t.Fatal("should be in special mode after 'jt'")
	}
	if len(tc.candidates) != 2 {
		t.Fatalf("expected 2 candidates for 'jt', got %d", len(tc.candidates))
	}

	// 按数字 '1' 选第一候选
	res = tc.pressKey("1")
	if res == nil {
		t.Fatal("result should not be nil")
	}
	if res.Type != bridge.ResponseTypeInsertText {
		t.Fatalf("expected InsertText, got %q", res.Type)
	}
	if res.Text != "→" {
		t.Errorf("expected →, got %q", res.Text)
	}
	if tc.specialMode {
		t.Error("specialMode should be false after selection")
	}
}

// TestSpecialMode_Backspace_EmptyBuffer 空 buffer 下退格 → 退出模式，返回 ClearComposition
func TestSpecialMode_Backspace_EmptyBuffer(t *testing.T) {
	tc := newSpecialTestCoordinator(t)
	enterSpecialMode(t, tc)

	// buffer 为空，退格应退出模式
	res := tc.pressKeyCode(int(ipc.VK_BACK))
	if res == nil {
		t.Fatal("result should not be nil")
	}
	if res.Type != bridge.ResponseTypeClearComposition {
		t.Fatalf("expected ClearComposition, got %q", res.Type)
	}
	if tc.specialMode {
		t.Error("specialMode should be false after backspace on empty buffer")
	}
}

// TestSpecialMode_Escape 按 Esc → 退出，返回 ClearComposition
func TestSpecialMode_Escape(t *testing.T) {
	tc := newSpecialTestCoordinator(t)
	enterSpecialMode(t, tc)

	// 输入一些字符后 Esc
	tc.pressKey("j")
	res := tc.pressKeyCode(int(ipc.VK_ESCAPE))
	if res == nil {
		t.Fatal("result should not be nil")
	}
	if res.Type != bridge.ResponseTypeClearComposition {
		t.Fatalf("expected ClearComposition, got %q", res.Type)
	}
	if tc.specialMode {
		t.Error("specialMode should be false after Esc")
	}
}

// TestSpecialMode_Backspace_NonEmpty 有 buffer 时退格删末字符，模式仍激活
func TestSpecialMode_Backspace_NonEmpty(t *testing.T) {
	tc := newSpecialTestCoordinator(t)
	enterSpecialMode(t, tc)

	tc.pressKey("j")
	tc.pressKey("t")
	if tc.specialBuffer != "jt" {
		t.Fatalf("expected buffer 'jt', got %q", tc.specialBuffer)
	}

	res := tc.pressKeyCode(int(ipc.VK_BACK))
	if res == nil {
		t.Fatal("result should not be nil")
	}
	if tc.specialBuffer != "j" {
		t.Errorf("expected buffer 'j' after backspace, got %q", tc.specialBuffer)
	}
	if !tc.specialMode {
		t.Error("specialMode should still be true")
	}
}

// TestSpecialMode_SpaceOnEmptyBuffer 空 buffer 时按空格 → 上屏触发符
func TestSpecialMode_SpaceOnEmptyBuffer(t *testing.T) {
	tc := newSpecialTestCoordinator(t)
	enterSpecialMode(t, tc)

	res := tc.pressKeyCode(int(ipc.VK_SPACE))
	if res == nil {
		t.Fatal("result should not be nil")
	}
	if res.Type != bridge.ResponseTypeInsertText {
		t.Fatalf("expected InsertText, got %q", res.Type)
	}
	if res.Text != "`" {
		t.Errorf("expected backtick, got %q", res.Text)
	}
	if tc.specialMode {
		t.Error("specialMode should be false after space on empty buffer")
	}
}

// TestSpecialMode_AA_ExpandsToFourCandidates 码表中 $AA("箭头","←↑→↓") 应展开为 4 候选；
// 按 '1' 上屏第一个字符 "←"。
// testdata 中 arrx 有两行（$AA + "→"），raw candCount=2 → 不自动上屏，可验证候选列表。
func TestSpecialMode_AA_ExpandsToFourCandidates(t *testing.T) {
	tc := newSpecialTestCoordinator(t)
	enterSpecialMode(t, tc)

	// 输入 "arrx"：testdata 中有 $AA("箭头","←↑→↓") 和 "→" 两行，candCount=2 不自动上屏
	for _, ch := range "arrx" {
		tc.pressKey(string(ch))
	}

	if !tc.specialMode {
		t.Fatal("should still be in special mode after 'arrx'")
	}
	// $AA 展开 4 个 + 字面量 "→" = 5 个
	if len(tc.candidates) != 5 {
		t.Fatalf("expected 5 candidates (4 from $AA + 1 literal), got %d", len(tc.candidates))
	}
	wantFirst4 := []string{"←", "↑", "→", "↓"}
	for i, want := range wantFirst4 {
		if tc.candidates[i].Text != want {
			t.Errorf("candidates[%d].Text = %q, want %q", i, tc.candidates[i].Text, want)
		}
	}

	// 按数字 '1' 选第一候选 "←"
	res := tc.pressKey("1")
	if res == nil {
		t.Fatal("result should not be nil")
	}
	if res.Type != bridge.ResponseTypeInsertText {
		t.Fatalf("expected InsertText, got %q", res.Type)
	}
	if res.Text != "←" {
		t.Errorf("expected ←, got %q", res.Text)
	}
	if tc.specialMode {
		t.Error("specialMode should be false after selection")
	}
}

// TestSpecialMode_RetriggerCommitsSymbol 进入后再次按引导符 ` → 作为符号上屏并退出
// （避免无法输入该符号本身）。
func TestSpecialMode_RetriggerCommitsSymbol(t *testing.T) {
	tc := newSpecialTestCoordinator(t)
	enterSpecialMode(t, tc)

	// 用真实 VK_OEM_3（避免 pressKey 把 ` 的 keycode 取成 96=VK_NUMPAD0 被小键盘拦截）。
	res := tc.HandleKeyEvent(bridge.KeyEventData{Key: "`", KeyCode: int(ipc.VK_OEM_3)})
	if tc.specialMode {
		t.Error("specialMode should be false after re-pressing trigger symbol")
	}
	if res == nil || res.Type != bridge.ResponseTypeInsertText {
		t.Fatalf("expected InsertText, got %+v", res)
	}
	if res.Text != "`" {
		t.Errorf("expected backtick committed, got %q", res.Text)
	}
}

// TestSpecialMode_PunctCommitsHighlight 打 "jt"(高亮 →) 后按标点 → 顶屏高亮候选 + 标点，退出。
func TestSpecialMode_PunctCommitsHighlight(t *testing.T) {
	tc := newSpecialTestCoordinator(t)
	enterSpecialMode(t, tc)

	tc.pressKey("j")
	tc.pressKey("t")
	if !tc.specialMode || len(tc.candidates) == 0 {
		t.Fatal("should be in mode with candidates after 'jt'")
	}

	res := tc.pressKey(",")
	if tc.specialMode {
		t.Error("specialMode should be false after punctuation commit")
	}
	if res == nil || res.Type != bridge.ResponseTypeInsertText {
		t.Fatalf("expected InsertText, got %+v", res)
	}
	rs := []rune(res.Text)
	if len(rs) < 1 || rs[0] != '→' {
		t.Errorf("expected committed highlight → as prefix, got %q", res.Text)
	}
}

// TestSpecialMode_FixedLengthAutoCommit fixed_length=2：打满 2 码且唯一候选时自动上屏。
func TestSpecialMode_FixedLengthAutoCommit(t *testing.T) {
	tc := newTestCoordinator(t)
	dir, _ := filepath.Abs("testdata")
	tc.specialModeReg = newSpecialModeRegistry(
		[]config.SpecialModeConfig{{
			ID: "rare", Name: "生僻字", TriggerKeys: []string{"grave"},
			Table: "special_symbols.dict.yaml", AutoCommit: config.SpecialAutoCommitFixedLength, FixedLength: 2,
		}},
		[]string{dir}, testSpecialLogger())

	if res := tc.HandleKeyEvent(bridge.KeyEventData{Key: "`", KeyCode: int(ipc.VK_OEM_3)}); res == nil {
		t.Fatal("enter result nil")
	}
	if !tc.specialMode {
		t.Fatal("should be in special mode after grave")
	}

	// "x"：长度 1 < 2，不自动上屏
	tc.pressKey("x")
	if !tc.specialMode {
		t.Fatal("should still be in mode after 'x' (len < fixed_length)")
	}

	// "h"：buffer "xh" 长度 2 且唯一候选 ① → 自动上屏
	res := tc.pressKey("h")
	if tc.specialMode {
		t.Error("specialMode should be false after fixed_length auto-commit")
	}
	if res == nil || res.Type != bridge.ResponseTypeInsertText || res.Text != "①" {
		t.Fatalf("expected InsertText ①, got %+v", res)
	}
}

// TestSpecialMode_DynamicPagingExpand 验证翻页动态加载：limit 翻倍重查，加载到表尾停止。
func TestSpecialMode_DynamicPagingExpand(t *testing.T) {
	tc := newTestCoordinator(t)
	dir, _ := filepath.Abs("testdata")
	tc.specialModeReg = newSpecialModeRegistry(
		[]config.SpecialModeConfig{{
			ID: "pg", TriggerKeys: []string{"grave"},
			Table: "special_paging.dict.yaml", AutoCommit: config.SpecialAutoCommitManual,
		}},
		[]string{dir}, testSpecialLogger())
	inst := tc.specialModeReg.get("pg")
	if _, err := tc.specialModeReg.ensureLoaded(inst); err != nil {
		t.Fatalf("ensureLoaded: %v", err)
	}

	// 模拟已进入、已加载前 4 条（prefix "z" 共 12 条）。
	tc.specialMode = true
	tc.specialActiveID = "pg"
	tc.specialBuffer = "z"
	tc.specialCandidateInput = "z"
	tc.specialCandidateLimit = 4
	tc.specialHasMore = true
	tc.candidates = tc.buildSpecialUICandidates(inst.table.LookupPrefix("z", 4))

	// 第一次扩展：4 → 8，仍有更多（12 > 8）。
	tc.expandSpecialCandidates()
	if tc.specialCandidateLimit != 8 || len(tc.candidates) != 8 {
		t.Fatalf("after 1st expand want limit=8/len=8, got limit=%d/len=%d", tc.specialCandidateLimit, len(tc.candidates))
	}
	if !tc.specialHasMore {
		t.Error("should still have more after loading 8/12")
	}

	// 第二次扩展：8 → 16，命中表尾 12 条，停止。
	tc.expandSpecialCandidates()
	if len(tc.candidates) != 12 {
		t.Fatalf("after 2nd expand want all 12, got %d", len(tc.candidates))
	}
	if tc.specialHasMore {
		t.Error("should have no more after loading all 12")
	}

	// 第三次扩展：已无更多，应为 no-op。
	tc.expandSpecialCandidates()
	if len(tc.candidates) != 12 {
		t.Fatalf("3rd expand should be no-op, got %d", len(tc.candidates))
	}
}

// TestSpecialMode_HotReloadRebuildsRegistry 验证 UpdateInputConfig 热重载会重建 registry，
// 使新增的 special_modes 配置立即生效（无需重启）。
func TestSpecialMode_HotReloadRebuildsRegistry(t *testing.T) {
	tc := newTestCoordinator(t)
	if tc.specialModeReg != nil && tc.specialModeReg.match("`", int(ipc.VK_OEM_3)) != "" {
		t.Fatal("no special mode expected before reload")
	}

	newInput := tc.config.Input
	newInput.SpecialModes = []config.SpecialModeConfig{
		{ID: "sym", Name: "快符", TriggerKeys: []string{"grave"}, Table: "x.dict.yaml", AutoCommit: "prefix_free"},
	}
	tc.UpdateInputConfig(&newInput)

	if tc.specialModeReg == nil {
		t.Fatal("specialModeReg should be rebuilt after hot reload")
	}
	if got := tc.specialModeReg.match("`", int(ipc.VK_OEM_3)); got != "sym" {
		t.Fatalf("special trigger should match after hot reload, got %q", got)
	}
}
