package config

import "testing"

func TestSpecialModeConfig_Validate(t *testing.T) {
	cases := []struct {
		name string
		cfg  SpecialModeConfig
		ok   bool
	}{
		{"ok_prefix_free", SpecialModeConfig{ID: "sym", TriggerKeys: []string{"grave"}, Table: "a.dict.yaml", AutoCommit: "prefix_free"}, true},
		{"ok_fixed_len", SpecialModeConfig{ID: "rare", TriggerKeys: []string{"semicolon"}, Table: "b.dict.yaml", AutoCommit: "fixed_length", FixedLength: 4}, true},
		{"ok_manual", SpecialModeConfig{ID: "m", TriggerKeys: []string{"quote"}, Table: "c.dict.yaml", AutoCommit: "manual"}, true},
		{"empty_id", SpecialModeConfig{ID: "", TriggerKeys: []string{"grave"}, Table: "a", AutoCommit: "manual"}, false},
		{"empty_triggers", SpecialModeConfig{ID: "x", TriggerKeys: nil, Table: "a", AutoCommit: "manual"}, false},
		{"empty_table", SpecialModeConfig{ID: "x", TriggerKeys: []string{"grave"}, Table: "", AutoCommit: "manual"}, false},
		{"bad_strategy", SpecialModeConfig{ID: "x", TriggerKeys: []string{"grave"}, Table: "a", AutoCommit: "weird"}, false},
		{"fixed_len_zero", SpecialModeConfig{ID: "x", TriggerKeys: []string{"grave"}, Table: "a", AutoCommit: "fixed_length", FixedLength: 0}, false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := tc.cfg.Validate()
			if tc.ok && err != nil {
				t.Fatalf("want ok, got err: %v", err)
			}
			if !tc.ok && err == nil {
				t.Fatalf("want err, got nil")
			}
		})
	}
}
