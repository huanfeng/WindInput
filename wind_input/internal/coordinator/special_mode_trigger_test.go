package coordinator

import (
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/pkg/config"
)

func TestTriggerModes_SpecialInserted(t *testing.T) {
	dir, _ := filepath.Abs("testdata")
	c := &Coordinator{}
	c.config = config.DefaultConfig()
	c.config.Input.SpecialModes = []config.SpecialModeConfig{
		{ID: "sym", Name: "快符", TriggerKeys: []string{"grave"}, Table: "special_symbols.dict.yaml", AutoCommit: "prefix_free"},
	}
	c.specialModeReg = newSpecialModeRegistry(c.config.Input.SpecialModes, []string{dir}, testSpecialLogger())

	modes := c.triggerModes()
	var names []string
	for _, m := range modes {
		names = append(names, m.name)
	}
	idxPinyin, idxSpecial, idxEng := indexOfStr(names, "temp_pinyin"), indexOfStr(names, "special:sym"), indexOfStr(names, "temp_english")
	if idxSpecial < 0 {
		t.Fatalf("special:sym not in modes: %v", names)
	}
	if !(idxPinyin < idxSpecial && idxSpecial < idxEng) {
		t.Fatalf("order wrong: %v (want temp_pinyin < special:sym < temp_english)", names)
	}
}

func indexOfStr(ss []string, s string) int {
	for i, v := range ss {
		if v == s {
			return i
		}
	}
	return -1
}
