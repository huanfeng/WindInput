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

func testSpecialLogger() *slog.Logger {
	return slog.New(slog.NewTextHandler(io.Discard, nil))
}
