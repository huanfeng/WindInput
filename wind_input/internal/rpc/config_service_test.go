package rpc

import (
	"encoding/json"
	"io"
	"log/slog"
	"sync"
	"testing"

	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/rpcapi"
)

// ── mockConfigReloader ──────────────────────────────────────────────────────

type mockConfigReloader struct{}

func (m *mockConfigReloader) ReloadConfig() error { return nil }
func (m *mockConfigReloader) ApplyConfigUpdate(oldCfg, newCfg *config.Config, _ map[string]bool) (bool, error) {
	*oldCfg = *newCfg
	return false, nil
}
func (m *mockConfigReloader) RebuildDictCache() (int, error) { return 0, nil }

// newTestConfigService 构建带 no-op saveFn 的 ConfigService，供各测试复用。
func newTestConfigService(cfg *config.Config) *ConfigService {
	return &ConfigService{
		cfgMu:          new(sync.RWMutex),
		cfg:            cfg,
		configReloader: &mockConfigReloader{},
		broadcaster:    NewEventBroadcaster(nil),
		logger:         slog.New(slog.NewTextHandler(io.Discard, nil)),
		saveFn:         func(*config.Config) error { return nil },
	}
}

// ── resolveKeyPath ──────────────────────────────────────────────────────────

func TestResolveKeyPath_Valid(t *testing.T) {
	cases := []struct {
		key     string
		section string
		path    []string
	}{
		{"ui.candidate.font_size", "ui", []string{"candidate", "font_size"}},
		{"input.auto_pair.chinese", "input", []string{"auto_pair", "chinese"}},
		{"debug.log_level", "debug", []string{"log_level"}},
	}
	for _, tc := range cases {
		sec, path, err := resolveKeyPath(tc.key)
		if err != nil {
			t.Errorf("%q: unexpected error: %v", tc.key, err)
			continue
		}
		if sec != tc.section {
			t.Errorf("%q: section want %q got %q", tc.key, tc.section, sec)
		}
		if len(path) != len(tc.path) {
			t.Errorf("%q: path len want %d got %d", tc.key, len(tc.path), len(path))
			continue
		}
		for i := range tc.path {
			if path[i] != tc.path[i] {
				t.Errorf("%q: path[%d] want %q got %q", tc.key, i, tc.path[i], path[i])
			}
		}
	}
}

func TestResolveKeyPath_Invalid(t *testing.T) {
	cases := []string{"noDot", ".bad", "bad.", ""}
	for _, key := range cases {
		_, _, err := resolveKeyPath(key)
		if err == nil {
			t.Errorf("%q: expected error, got nil", key)
		}
	}
}

// ── getSectionMap / setSectionFromMap ───────────────────────────────────────

func TestGetSectionMap_AllSections(t *testing.T) {
	cfg := config.SystemDefaultConfig()
	sections := []string{"general", "schema", "hotkeys", "input", "ui", "features", "compat", "debug"}
	for _, sec := range sections {
		m, err := getSectionMap(cfg, sec)
		if err != nil {
			t.Errorf("section %q: %v", sec, err)
			continue
		}
		if m == nil {
			t.Errorf("section %q: got nil map", sec)
		}
	}
}

func TestGetSectionMap_Unknown(t *testing.T) {
	cfg := config.SystemDefaultConfig()
	_, err := getSectionMap(cfg, "nonexistent")
	if err == nil {
		t.Error("expected error for unknown section")
	}
}

func TestSetSectionRoundTrip(t *testing.T) {
	cfg := config.SystemDefaultConfig()
	m, _ := getSectionMap(cfg, "ui")
	if err := setNestedKey(m, []string{"candidate", "font_size"}, float64(18)); err != nil {
		t.Fatalf("setNestedKey: %v", err)
	}
	if err := setSectionFromMap(cfg, "ui", m); err != nil {
		t.Fatalf("setSectionFromMap: %v", err)
	}
	if cfg.UI.Candidate.FontSize != 18 {
		t.Errorf("expected font_size=18, got %v", cfg.UI.Candidate.FontSize)
	}
}

// ── getNestedKey / setNestedKey ─────────────────────────────────────────────

func TestGetNestedKey_Flat(t *testing.T) {
	m := map[string]any{"key": "value"}
	v, err := getNestedKey(m, []string{"key"})
	if err != nil || v != "value" {
		t.Errorf("expected 'value', got %v, err=%v", v, err)
	}
}

func TestGetNestedKey_Nested(t *testing.T) {
	m := map[string]any{"a": map[string]any{"b": float64(42)}}
	v, err := getNestedKey(m, []string{"a", "b"})
	if err != nil || v != float64(42) {
		t.Errorf("expected 42, got %v, err=%v", v, err)
	}
}

func TestGetNestedKey_Missing(t *testing.T) {
	m := map[string]any{"a": map[string]any{}}
	_, err := getNestedKey(m, []string{"a", "missing"})
	if err == nil {
		t.Error("expected error for missing key")
	}
}

func TestSetNestedKey_Flat(t *testing.T) {
	m := map[string]any{}
	setNestedKey(m, []string{"x"}, 99)
	if m["x"] != 99 {
		t.Errorf("expected 99, got %v", m["x"])
	}
}

func TestSetNestedKey_DeepNested(t *testing.T) {
	m := map[string]any{}
	setNestedKey(m, []string{"a", "b", "c"}, "deep")
	v, err := getNestedKey(m, []string{"a", "b", "c"})
	if err != nil || v != "deep" {
		t.Errorf("expected 'deep', got %v, err=%v", v, err)
	}
}

func TestSetNestedKey_Overwrite(t *testing.T) {
	m := map[string]any{"a": map[string]any{"b": "old"}}
	setNestedKey(m, []string{"a", "b"}, "new")
	v, _ := getNestedKey(m, []string{"a", "b"})
	if v != "new" {
		t.Errorf("expected 'new', got %v", v)
	}
}

// ── diffSections ────────────────────────────────────────────────────────────

func TestDiffSections_NoChange(t *testing.T) {
	cfg := config.SystemDefaultConfig()
	cfg2 := config.SystemDefaultConfig()
	diff := diffSections(cfg, cfg2)
	if len(diff) != 0 {
		t.Errorf("expected no diff, got %v", diff)
	}
}

func TestDiffSections_UIChanged(t *testing.T) {
	cfg := config.SystemDefaultConfig()
	cfg2 := config.SystemDefaultConfig()
	cfg2.UI.Candidate.FontSize = cfg.UI.Candidate.FontSize + 4
	diff := diffSections(cfg, cfg2)
	if !diff["ui"] {
		t.Error("expected ui section to be in diff")
	}
	if diff["input"] || diff["hotkeys"] {
		t.Errorf("unexpected sections in diff: %v", diff)
	}
}

func TestDiffSections_MultipleSections(t *testing.T) {
	cfg := config.SystemDefaultConfig()
	cfg2 := config.SystemDefaultConfig()
	cfg2.UI.Candidate.FontSize++
	cfg2.Input.FilterMode = "none"
	diff := diffSections(cfg, cfg2)
	if !diff["ui"] || !diff["input"] {
		t.Errorf("expected ui+input in diff, got %v", diff)
	}
}

// ── ConfigService: GetAll / Get / GetDefaults ───────────────────────────────

func TestConfigGetAll(t *testing.T) {
	cfg := config.SystemDefaultConfig()
	svc := newTestConfigService(cfg)

	var reply rpcapi.ConfigGetAllReply
	if err := svc.GetAll(&rpcapi.Empty{}, &reply); err != nil {
		t.Fatalf("GetAll: %v", err)
	}
	if len(reply.Config) == 0 {
		t.Error("expected non-empty config JSON")
	}
	var m map[string]any
	if err := json.Unmarshal(reply.Config, &m); err != nil {
		t.Fatalf("invalid JSON from GetAll: %v", err)
	}
	if _, ok := m["ui"]; !ok {
		t.Error("expected 'ui' key in config")
	}
}

func TestConfigGet_ValidKey(t *testing.T) {
	cfg := config.SystemDefaultConfig()
	cfg.UI.Candidate.FontSize = 16
	svc := newTestConfigService(cfg)

	var reply rpcapi.ConfigGetReply
	if err := svc.Get(&rpcapi.ConfigGetArgs{Keys: []string{"ui.candidate.font_size"}}, &reply); err != nil {
		t.Fatalf("Get: %v", err)
	}
	v, ok := reply.Values["ui.candidate.font_size"]
	if !ok {
		t.Fatal("key ui.font_size not in reply")
	}
	if v != float64(16) {
		t.Errorf("expected 16, got %v", v)
	}
}

func TestConfigGet_UnknownSection(t *testing.T) {
	cfg := config.SystemDefaultConfig()
	svc := newTestConfigService(cfg)

	var reply rpcapi.ConfigGetReply
	err := svc.Get(&rpcapi.ConfigGetArgs{Keys: []string{"unknown.field"}}, &reply)
	if err == nil {
		t.Error("expected error for unknown section")
	}
}

func TestConfigGet_InvalidKeyFormat(t *testing.T) {
	cfg := config.SystemDefaultConfig()
	svc := newTestConfigService(cfg)

	var reply rpcapi.ConfigGetReply
	err := svc.Get(&rpcapi.ConfigGetArgs{Keys: []string{"noDot"}}, &reply)
	if err == nil {
		t.Error("expected error for key without dot")
	}
}

func TestConfigGetDefaults(t *testing.T) {
	cfg := config.SystemDefaultConfig()
	svc := newTestConfigService(cfg)

	var reply rpcapi.ConfigGetDefaultsReply
	if err := svc.GetDefaults(&rpcapi.Empty{}, &reply); err != nil {
		t.Fatalf("GetDefaults: %v", err)
	}
	if len(reply.Config) == 0 {
		t.Error("expected non-empty defaults JSON")
	}
}

// ── ConfigService: Set / SetAll / Reset ────────────────────────────────────

func TestConfigSet_SingleKey(t *testing.T) {
	cfg := config.SystemDefaultConfig()
	cfg.UI.Candidate.FontSize = 12
	svc := newTestConfigService(cfg)

	var reply rpcapi.ConfigSetReply
	err := svc.Set(&rpcapi.ConfigSetArgs{
		Items: []rpcapi.ConfigSetItem{{Key: "ui.candidate.font_size", Value: float64(20)}},
	}, &reply)
	if err != nil {
		t.Fatalf("Set: %v", err)
	}
	if len(reply.Applied) != 1 || reply.Applied[0] != "ui.candidate.font_size" {
		t.Errorf("unexpected Applied: %v", reply.Applied)
	}
	if cfg.UI.Candidate.FontSize != 20 {
		t.Errorf("expected font_size=20 in live config, got %v", cfg.UI.Candidate.FontSize)
	}
}

func TestConfigSet_MultipleKeys(t *testing.T) {
	cfg := config.SystemDefaultConfig()
	svc := newTestConfigService(cfg)

	var reply rpcapi.ConfigSetReply
	err := svc.Set(&rpcapi.ConfigSetArgs{
		Items: []rpcapi.ConfigSetItem{
			{Key: "ui.candidate.font_size", Value: float64(14)},
			{Key: "ui.toolbar.visible", Value: true},
		},
	}, &reply)
	if err != nil {
		t.Fatalf("Set: %v", err)
	}
	if len(reply.Applied) != 2 {
		t.Errorf("expected 2 applied, got %d", len(reply.Applied))
	}
}

func TestConfigSet_InvalidKey(t *testing.T) {
	cfg := config.SystemDefaultConfig()
	svc := newTestConfigService(cfg)

	var reply rpcapi.ConfigSetReply
	err := svc.Set(&rpcapi.ConfigSetArgs{
		Items: []rpcapi.ConfigSetItem{{Key: "badkey", Value: 1}},
	}, &reply)
	if err == nil {
		t.Error("expected error for invalid key")
	}
}

func TestConfigSetAll(t *testing.T) {
	cfg := config.SystemDefaultConfig()
	svc := newTestConfigService(cfg)

	newCfg := config.SystemDefaultConfig()
	newCfg.UI.Candidate.FontSize = 22
	data, _ := json.Marshal(newCfg)

	var reply rpcapi.ConfigSetAllReply
	if err := svc.SetAll(&rpcapi.ConfigSetAllArgs{Config: data}, &reply); err != nil {
		t.Fatalf("SetAll: %v", err)
	}
	if cfg.UI.Candidate.FontSize != 22 {
		t.Errorf("expected font_size=22 after SetAll, got %v", cfg.UI.Candidate.FontSize)
	}
}

// TestConfigSetAll_PreservesStats 防回归：全局保存（SetAll）提交的 formData 不含 stats 字段
// （前端表单零值序列化），SetAll 必须保留服务端现有 stats，
// 否则会把用户在统计页关闭的 track_english=false 冲回默认 true。
func TestConfigSetAll_PreservesStats(t *testing.T) {
	cfg := config.SystemDefaultConfig()
	cfg.Features.Stats.Enabled = true
	cfg.Features.Stats.TrackEnglish = false // 用户在统计页关闭了英文统计
	svc := newTestConfigService(cfg)

	// 模拟前端全局保存：formData 的 stats 为零值（enabled=false 等并非用户意图）
	frontendCfg := config.SystemDefaultConfig()
	frontendCfg.Features.Stats = config.StatsConfig{}
	frontendCfg.UI.Candidate.FontSize = 22 // 同时修改一个全局表单字段
	data, _ := json.Marshal(frontendCfg)

	var reply rpcapi.ConfigSetAllReply
	if err := svc.SetAll(&rpcapi.ConfigSetAllArgs{Config: data}, &reply); err != nil {
		t.Fatalf("SetAll: %v", err)
	}

	if cfg.UI.Candidate.FontSize != 22 {
		t.Errorf("全局表单字段未生效：font_size want 22, got %v", cfg.UI.Candidate.FontSize)
	}
	if cfg.Features.Stats.TrackEnglish != false {
		t.Errorf("track_english 被全局保存覆盖：want false, got %v", cfg.Features.Stats.TrackEnglish)
	}
	if cfg.Features.Stats.Enabled != true {
		t.Errorf("stats.enabled 被全局保存覆盖：want true, got %v", cfg.Features.Stats.Enabled)
	}
}

// TestConfigSetAll_TypeMismatchTolerant 防回归：单个字段类型不匹配（如 ui.font_size 传成字符串）
// 不应让整体保存失败。encoding/json 会部分填充 newCfg，SetAll 应记录字段名并继续保存。
func TestConfigSetAll_TypeMismatchTolerant(t *testing.T) {
	cfg := config.SystemDefaultConfig()
	saved := false
	svc := newTestConfigService(cfg)
	svc.saveFn = func(*config.Config) error {
		saved = true
		return nil
	}

	// 合法 JSON，但 ui.candidate.font_size 被写成字符串（应是数字）→ *json.UnmarshalTypeError，
	// 同时 ui.theme.name 等其余字段仍可被部分填充。
	data := []byte(`{"ui":{"candidate":{"font_size":"not-a-number"},"theme":{"name":"custom-theme"}}}`)

	var reply rpcapi.ConfigSetAllReply
	if err := svc.SetAll(&rpcapi.ConfigSetAllArgs{Config: data}, &reply); err != nil {
		t.Fatalf("SetAll 应容忍单字段类型不匹配，got err: %v", err)
	}
	if !saved {
		t.Error("saveFn 未被调用：类型不匹配时仍应保存部分填充的配置")
	}
	if cfg.UI.Theme.Name != "custom-theme" {
		t.Errorf("类型不匹配字段之外的字段应被部分填充：theme want %q got %q", "custom-theme", cfg.UI.Theme.Name)
	}
}

func TestConfigReset_ToDefault(t *testing.T) {
	def := config.SystemDefaultConfig()
	cfg := config.SystemDefaultConfig()
	cfg.UI.Candidate.FontSize = 99
	svc := newTestConfigService(cfg)

	var reply rpcapi.ConfigResetReply
	if err := svc.Reset(&rpcapi.ConfigResetArgs{Keys: []string{"ui.candidate.font_size"}}, &reply); err != nil {
		t.Fatalf("Reset: %v", err)
	}
	if cfg.UI.Candidate.FontSize != def.UI.Candidate.FontSize {
		t.Errorf("expected font_size reset to %v, got %v", def.UI.Candidate.FontSize, cfg.UI.Candidate.FontSize)
	}
}

func TestConfigSetActiveSchema(t *testing.T) {
	cfg := config.SystemDefaultConfig()
	cfg.Schema.Available = []string{"wubi", "pinyin"}
	cfg.Schema.Active = "wubi"
	svc := newTestConfigService(cfg)

	var empty rpcapi.Empty
	if err := svc.SetActiveSchema(&rpcapi.SetActiveSchemaArgs{SchemaID: "pinyin"}, &empty); err != nil {
		t.Fatalf("SetActiveSchema: %v", err)
	}
	if cfg.Schema.Active != "pinyin" {
		t.Errorf("expected active=pinyin, got %q", cfg.Schema.Active)
	}
}

// TestConfigSet_PunctCustomMappings_ClearAll 验证清空 punct_custom.mappings 后旧条目不残留。
// 回归保护：setSectionFromMap 必须先清零 section 再 Unmarshal，否则 encoding/json 的 map
// 合并语义会让 {} 保留已有条目（Bug：恢复默认后保存成功但重开对话框还是旧数据）。
func TestConfigSet_PunctCustomMappings_ClearAll(t *testing.T) {
	cfg := config.SystemDefaultConfig()
	cfg.Input.PunctCustom.Enabled = true
	cfg.Input.PunctCustom.Mappings = map[string][]string{
		";": {"；", "", "", ""},
		",": {"，", "", "", ""},
	}
	svc := newTestConfigService(cfg)

	// 发送 mappings = {} 清空所有条目（等同于前端"恢复默认"后保存）
	var reply rpcapi.ConfigSetReply
	err := svc.Set(&rpcapi.ConfigSetArgs{
		Items: []rpcapi.ConfigSetItem{
			{Key: "input.punct_custom.mappings", Value: map[string]any{}},
		},
	}, &reply)
	if err != nil {
		t.Fatalf("Set: %v", err)
	}

	if len(cfg.Input.PunctCustom.Mappings) != 0 {
		t.Errorf("清空 mappings 后应为空，got %v", cfg.Input.PunctCustom.Mappings)
	}
}

// TestConfigSet_PunctCustomMappings_PartialRemove 验证移除部分条目后只保留剩余条目。
func TestConfigSet_PunctCustomMappings_PartialRemove(t *testing.T) {
	cfg := config.SystemDefaultConfig()
	cfg.Input.PunctCustom.Enabled = true
	cfg.Input.PunctCustom.Mappings = map[string][]string{
		";": {"；", "", "", ""},
		",": {"，", "", "", ""},
	}
	svc := newTestConfigService(cfg)

	// 只保留 ";" 条目，去掉 ","
	var reply rpcapi.ConfigSetReply
	err := svc.Set(&rpcapi.ConfigSetArgs{
		Items: []rpcapi.ConfigSetItem{
			{Key: "input.punct_custom.mappings", Value: map[string]any{
				";": []any{"；", "", "", ""},
			}},
		},
	}, &reply)
	if err != nil {
		t.Fatalf("Set: %v", err)
	}

	if len(cfg.Input.PunctCustom.Mappings) != 1 {
		t.Errorf("应剩 1 个条目，got %v", cfg.Input.PunctCustom.Mappings)
	}
	if _, ok := cfg.Input.PunctCustom.Mappings[","]; ok {
		t.Error("',' 条目应已被删除")
	}
	if v := cfg.Input.PunctCustom.Mappings[";"]; len(v) == 0 {
		t.Error("';' 条目应被保留")
	}
}

// TestConfigSet_StatsTrackEnglish 验证 stats 配置走通用 Config.Set（按 key）：
// bool 字段经 setNestedKey/setSectionFromMap 往返正确：设 false 得 false，
// 且未改的其它 stats 字段（enabled）保留默认。
func TestConfigSet_StatsTrackEnglish(t *testing.T) {
	cfg := config.SystemDefaultConfig() // 默认 track_english/enabled 均为 true
	svc := newTestConfigService(cfg)

	var reply rpcapi.ConfigSetReply
	if err := svc.Set(&rpcapi.ConfigSetArgs{
		Items: []rpcapi.ConfigSetItem{{Key: "features.stats.track_english", Value: false}},
	}, &reply); err != nil {
		t.Fatalf("Set: %v", err)
	}

	if cfg.Features.Stats.TrackEnglish != false {
		t.Errorf("track_english 应为 false，got %v", cfg.Features.Stats.TrackEnglish)
	}
	if cfg.Features.Stats.Enabled != true {
		t.Errorf("未改的 enabled 应保留默认 true，got %v", cfg.Features.Stats.Enabled)
	}
}
