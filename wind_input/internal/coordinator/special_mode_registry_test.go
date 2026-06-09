package coordinator

import (
	"io"
	"log/slog"
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/pkg/config"
)

func TestSpecialModeRegistry_MatchAndLoad(t *testing.T) {
	dir, _ := filepath.Abs("testdata")
	reg := newSpecialModeRegistry([]config.SpecialModeConfig{
		{ID: "sym", Name: "快符", TriggerKeys: []string{"grave"}, Table: "special_symbols.dict.yaml", AutoCommit: "prefix_free"},
	}, []string{dir}, testSpecialLogger())

	if id := reg.match("`", 0xC0); id != "sym" {
		t.Fatalf("match grave want sym, got %q", id)
	}
	if id := reg.match("a", 0x41); id != "" {
		t.Fatalf("match 'a' want empty, got %q", id)
	}

	inst := reg.get("sym")
	if inst == nil {
		t.Fatal("get(sym) nil")
	}
	tbl, err := reg.ensureLoaded(inst)
	if err != nil {
		t.Fatalf("ensureLoaded: %v", err)
	}
	if got := tbl.Lookup("jt"); len(got) != 2 {
		t.Fatalf("Lookup(jt) want 2 cands, got %d", len(got))
	}
	if !tbl.HasLongerCode("arr") {
		t.Fatalf("HasLongerCode(arr) want true")
	}
	if tbl.HasLongerCode("arrow") {
		t.Fatalf("HasLongerCode(arrow) want false")
	}
}

// TestSpecialModeRegistry_AllCandidates 验证 wdb 路径下 AllCandidates 能列出整张码表
// （wdb 的 LookupPrefix("") 返回 nil，AllCandidates 走 scanPrefix("") 绕过；
// 这是 ShowAllOnEntry「进入即列全部」的依赖）。
func TestSpecialModeRegistry_AllCandidates(t *testing.T) {
	dir, _ := filepath.Abs("testdata")
	reg := newSpecialModeRegistry([]config.SpecialModeConfig{
		{ID: "sym", TriggerKeys: []string{"grave"}, Table: "special_symbols.dict.yaml", AutoCommit: "prefix_free"},
	}, []string{dir}, testSpecialLogger())
	tbl, err := reg.ensureLoaded(reg.get("sym"))
	if err != nil {
		t.Fatalf("ensureLoaded: %v", err)
	}
	// 夹具至少含 jt(→/←)、xh(①)、arrow(⇧) 等多条；AllCandidates 应远多于 0。
	all := tbl.AllCandidates(200)
	if len(all) < 4 {
		t.Fatalf("AllCandidates want >=4 entries, got %d", len(all))
	}
}

func testSpecialLogger() *slog.Logger {
	return slog.New(slog.NewTextHandler(io.Discard, nil))
}
