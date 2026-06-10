package config

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"gopkg.in/yaml.v3"
)

// ---- TOML 桥接编解码 ----

// DefaultConfig 经 TOML 往返后必须与原值语义一致（用 yaml.Marshal 字节比较）。
func TestTOMLBridge_DefaultConfigRoundTrip(t *testing.T) {
	orig := DefaultConfig()

	data, err := marshalTOML(orig)
	if err != nil {
		t.Fatalf("marshalTOML 失败: %v", err)
	}

	got := DefaultConfig()
	yamlData, err := normalizeToYAML("config.toml", data)
	if err != nil {
		t.Fatalf("normalizeToYAML 失败: %v", err)
	}
	if err := yaml.Unmarshal(yamlData, got); err != nil {
		t.Fatalf("回读失败: %v", err)
	}

	origYAML, _ := yaml.Marshal(orig)
	gotYAML, _ := yaml.Marshal(got)
	if !bytes.Equal(origYAML, gotYAML) {
		t.Fatalf("TOML 往返后不一致:\n--- 原值 ---\n%s\n--- 往返后 ---\n%s", origYAML, gotYAML)
	}
}

// 空 TOML 文件应视为"无用户覆盖"，正常返回默认配置。
func TestLoadFrom_EmptyTOML(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "config.toml")
	if err := os.WriteFile(path, []byte{}, 0644); err != nil {
		t.Fatal(err)
	}

	cfg, err := LoadFrom(path)
	if err != nil {
		t.Fatalf("空文件不应报错: %v", err)
	}
	if cfg.UI.CandidatesPerPage != 7 {
		t.Fatalf("空文件应返回默认配置, candidates_per_page=%d", cfg.UI.CandidatesPerPage)
	}
}

// TOML 用户配置覆盖应只改写文件中出现的键，其余字段保持默认（部分覆盖语义）。
func TestLoadFrom_TOMLUserOverlay(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "config.toml")
	content := `
[ui]
font_size = 22.5

[input.temp_pinyin]
trigger_keys = ["semicolon"]
`
	if err := os.WriteFile(path, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	cfg, err := LoadFrom(path)
	if err != nil {
		t.Fatalf("LoadFrom 失败: %v", err)
	}
	if cfg.UI.FontSize != 22.5 {
		t.Errorf("font_size = %v, want 22.5", cfg.UI.FontSize)
	}
	if len(cfg.Input.TempPinyin.TriggerKeys) != 1 || cfg.Input.TempPinyin.TriggerKeys[0] != "semicolon" {
		t.Errorf("temp_pinyin.trigger_keys = %v, want [semicolon]", cfg.Input.TempPinyin.TriggerKeys)
	}
	// 未出现的键保持默认
	if !cfg.Hotkeys.CommitOnSwitch {
		t.Error("未覆盖字段 commit_on_switch 应保持默认 true")
	}
	if cfg.UI.CandidatesPerPage != 7 {
		t.Errorf("未覆盖字段 candidates_per_page = %d, want 7", cfg.UI.CandidatesPerPage)
	}
}

// 三态字段：TOML 中键缺失 = 未设置（取默认），显式 false = 用户关闭。
func TestLoadFrom_TOMLTriStateBool(t *testing.T) {
	dir := t.TempDir()

	// 键缺失 → nil → 默认 true
	path := filepath.Join(dir, "config.toml")
	if err := os.WriteFile(path, []byte("[toolbar]\nvisible = true\n"), 0644); err != nil {
		t.Fatal(err)
	}
	cfg, err := LoadFrom(path)
	if err != nil {
		t.Fatal(err)
	}
	if !cfg.Toolbar.IsHideInFullscreen() {
		t.Error("hide_in_fullscreen 缺失时应默认 true")
	}

	// 显式 false → 用户关闭
	if err := os.WriteFile(path, []byte("[toolbar]\nhide_in_fullscreen = false\n"), 0644); err != nil {
		t.Fatal(err)
	}
	cfg, err = LoadFrom(path)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Toolbar.IsHideInFullscreen() {
		t.Error("hide_in_fullscreen 显式 false 应生效")
	}
}

// ---- diff 保存 ----

// SaveTo 到 .toml 路径应只写入与系统默认的差异字段。
func TestSaveTo_TOMLDiffOnly(t *testing.T) {
	setTestConfigDir(t)
	dir := t.TempDir()
	path := filepath.Join(dir, "config.toml")

	cfg := SystemDefaultConfig()
	cfg.UI.FontSize = 30

	if err := SaveTo(cfg, path); err != nil {
		t.Fatalf("SaveTo 失败: %v", err)
	}

	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	content := string(data)
	if !strings.Contains(content, "font_size") {
		t.Errorf("diff 文件应包含 font_size, 实际:\n%s", content)
	}
	if strings.Contains(content, "candidates_per_page") {
		t.Errorf("diff 文件不应包含未修改的 candidates_per_page, 实际:\n%s", content)
	}

	// 回读验证
	loaded, err := LoadFrom(path)
	if err != nil {
		t.Fatal(err)
	}
	if loaded.UI.FontSize != 30 {
		t.Errorf("回读 font_size = %v, want 30", loaded.UI.FontSize)
	}
}

// 与系统默认完全一致时，diff 只含 version 元数据（见 version_test.go 的
// TestSaveTo_EmptyDiffWritesVersion），且可正常回读。
func TestSaveTo_TOMLEmptyDiff(t *testing.T) {
	setTestConfigDir(t)
	dir := t.TempDir()
	path := filepath.Join(dir, "config.toml")

	if err := SaveTo(SystemDefaultConfig(), path); err != nil {
		t.Fatalf("SaveTo 失败: %v", err)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Contains(data, []byte("version")) {
		t.Errorf("无差异时应写出 version 元数据, 实际:\n%s", data)
	}
	if _, err := LoadFrom(path); err != nil {
		t.Fatalf("空 diff 文件回读失败: %v", err)
	}
}

// ---- 旧版 YAML 迁移 ----

// 目标 .toml 不存在而旧版 .yaml 存在时：从旧文件加载、写出 TOML、旧文件改名备份。
func TestLoadFrom_MigratesLegacyYAML(t *testing.T) {
	setTestConfigDir(t)
	dir := t.TempDir()
	tomlPath := filepath.Join(dir, "config.toml")
	yamlPath := filepath.Join(dir, "config.yaml")

	legacy := "ui:\n  font_size: 25\nschema:\n  active: pinyin\n  available: [pinyin, wubi86]\n"
	if err := os.WriteFile(yamlPath, []byte(legacy), 0644); err != nil {
		t.Fatal(err)
	}

	cfg, err := LoadFrom(tomlPath)
	if err != nil {
		t.Fatalf("LoadFrom 失败: %v", err)
	}
	if cfg.UI.FontSize != 25 {
		t.Errorf("迁移加载 font_size = %v, want 25", cfg.UI.FontSize)
	}
	if cfg.Schema.Active != "pinyin" {
		t.Errorf("迁移加载 schema.active = %q, want pinyin", cfg.Schema.Active)
	}

	// TOML 已写出
	if _, err := os.Stat(tomlPath); err != nil {
		t.Errorf("迁移后 config.toml 应存在: %v", err)
	}
	// 旧文件已改名备份
	if _, err := os.Stat(yamlPath); !os.IsNotExist(err) {
		t.Error("迁移后 config.yaml 应已改名")
	}
	if _, err := os.Stat(yamlPath + MigratedBackupSuffix); err != nil {
		t.Errorf("迁移备份 config.yaml.migrated.bak 应存在: %v", err)
	}

	// 二次加载直读 TOML，结果一致
	cfg2, err := LoadFrom(tomlPath)
	if err != nil {
		t.Fatal(err)
	}
	if cfg2.UI.FontSize != 25 || cfg2.Schema.Active != "pinyin" {
		t.Errorf("二次加载结果不一致: font_size=%v active=%q", cfg2.UI.FontSize, cfg2.Schema.Active)
	}
}

// ---- 损坏与类型错误自愈 ----

// TOML 语法损坏：回退默认配置，备份原文件，自愈写出规范文件。
func TestLoadFrom_CorruptedTOML_SelfHeal(t *testing.T) {
	setTestConfigDir(t)
	dir := t.TempDir()
	path := filepath.Join(dir, "config.toml")
	if err := os.WriteFile(path, []byte("= broken [[ toml"), 0644); err != nil {
		t.Fatal(err)
	}

	cfg, err := LoadFrom(path)
	if err != nil {
		t.Fatalf("损坏文件应自愈而非报错: %v", err)
	}
	if cfg.UI.CandidatesPerPage != 7 {
		t.Errorf("损坏后应回退默认, candidates_per_page=%d", cfg.UI.CandidatesPerPage)
	}
	if _, err := os.Stat(path + ".bak"); err != nil {
		t.Errorf("损坏原文件应备份为 .bak: %v", err)
	}
	// 自愈后的文件可正常加载
	if _, err := LoadFrom(path); err != nil {
		t.Fatalf("自愈写回的文件应可加载: %v", err)
	}
}

// TOML 语法合法但字段类型不符：保留可解析字段（yaml.TypeError 部分解码语义）。
func TestLoadFrom_TOMLTypeError_PartialDecode(t *testing.T) {
	setTestConfigDir(t)
	dir := t.TempDir()
	path := filepath.Join(dir, "config.toml")
	content := `
[ui]
font_size = "not-a-number"
candidates_per_page = 9
`
	if err := os.WriteFile(path, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	cfg, err := LoadFrom(path)
	if err != nil {
		t.Fatalf("类型错误应自愈而非报错: %v", err)
	}
	if cfg.UI.CandidatesPerPage != 9 {
		t.Errorf("类型错误时其余字段应保留, candidates_per_page=%d want 9", cfg.UI.CandidatesPerPage)
	}
	if cfg.UI.FontSize != 18 {
		t.Errorf("出错字段应维持默认, font_size=%v want 18", cfg.UI.FontSize)
	}
}

// ---- RuntimeState ----

// state 保存为 TOML 后回读，含带逗号引号键的嵌套 map。
func TestRuntimeState_TOMLRoundTrip(t *testing.T) {
	configDir := setTestConfigDir(t)

	state := DefaultRuntimeState()
	state.ChineseMode = false
	state.EngineType = "codetable"
	state.ToolbarPositions = map[string][2]int{"1920,1040": {100, 200}}
	state.CandidatePinPositions = map[string]map[string][2]int{
		"wps.exe": {"2560,1400": {300, 400}},
	}

	if err := SaveRuntimeState(state); err != nil {
		t.Fatalf("SaveRuntimeState 失败: %v", err)
	}
	if _, err := os.Stat(filepath.Join(configDir, StateFileName)); err != nil {
		t.Fatalf("state.toml 应存在: %v", err)
	}

	loaded, err := LoadRuntimeState()
	if err != nil {
		t.Fatalf("LoadRuntimeState 失败: %v", err)
	}
	if loaded.ChineseMode || loaded.EngineType != "codetable" {
		t.Errorf("基础字段不一致: %+v", loaded)
	}
	if got := loaded.ToolbarPositions["1920,1040"]; got != [2]int{100, 200} {
		t.Errorf("toolbar_positions 引号键不一致: %v", got)
	}
	if got := loaded.CandidatePinPositions["wps.exe"]["2560,1400"]; got != [2]int{300, 400} {
		t.Errorf("candidate_pin_positions 嵌套引号键不一致: %v", got)
	}
}

// 旧版 state.yaml 自动迁移为 state.toml。
func TestRuntimeState_MigratesLegacyYAML(t *testing.T) {
	configDir := setTestConfigDir(t)
	if err := os.MkdirAll(configDir, 0755); err != nil {
		t.Fatal(err)
	}

	legacy := "chinese_mode: false\nengine_type: codetable\ntoolbar_positions:\n  \"1920,1040\": [11, 22]\n"
	yamlPath := filepath.Join(configDir, LegacyStateFileName)
	if err := os.WriteFile(yamlPath, []byte(legacy), 0644); err != nil {
		t.Fatal(err)
	}

	state, err := LoadRuntimeState()
	if err != nil {
		t.Fatalf("LoadRuntimeState 失败: %v", err)
	}
	if state.ChineseMode || state.EngineType != "codetable" {
		t.Errorf("旧版字段未生效: %+v", state)
	}
	if got := state.ToolbarPositions["1920,1040"]; got != [2]int{11, 22} {
		t.Errorf("toolbar_positions = %v, want [11 22]", got)
	}

	if _, err := os.Stat(filepath.Join(configDir, StateFileName)); err != nil {
		t.Errorf("迁移后 state.toml 应存在: %v", err)
	}
	if _, err := os.Stat(yamlPath); !os.IsNotExist(err) {
		t.Error("迁移后 state.yaml 应已改名")
	}
}

// ---- AppCompat ----

// 旧版用户 compat.yaml 在 toggle 写出时迁移为 compat.toml。
func TestCompat_ToggleMigratesLegacyYAML(t *testing.T) {
	configDir := setTestConfigDir(t)
	if err := os.MkdirAll(configDir, 0755); err != nil {
		t.Fatal(err)
	}

	legacy := "apps:\n  - process: \"Foo.exe\"\n    skip_caret_pending: false\n"
	yamlPath := filepath.Join(configDir, LegacyCompatFileName)
	if err := os.WriteFile(yamlPath, []byte(legacy), 0644); err != nil {
		t.Fatal(err)
	}

	newVal, err := ToggleUserSkipCaretPending("Foo.exe")
	if err != nil {
		t.Fatalf("Toggle 失败: %v", err)
	}
	if !newVal {
		t.Error("false 规则 toggle 后应为 true")
	}

	if _, err := os.Stat(filepath.Join(configDir, CompatFileName)); err != nil {
		t.Errorf("toggle 后 compat.toml 应存在: %v", err)
	}
	if _, err := os.Stat(yamlPath); !os.IsNotExist(err) {
		t.Error("toggle 后 compat.yaml 应已改名")
	}

	// 回读验证规则生效
	compat := LoadAppCompat()
	rule := compat.GetRule("foo.exe")
	if rule == nil || !rule.SkipCaretPending {
		t.Errorf("回读规则不正确: %+v", rule)
	}
}

// ---- SchemaOverrides ----

// 旧版 schema_overrides.yaml 回退读取；保存时迁移为 TOML。
func TestSchemaOverrides_LegacyYAMLFallbackAndMigration(t *testing.T) {
	configDir := setTestConfigDir(t)
	if err := os.MkdirAll(configDir, 0755); err != nil {
		t.Fatal(err)
	}

	legacy := "wubi86:\n  learning:\n    temp_promote_count: 0\n"
	yamlPath := filepath.Join(configDir, LegacySchemaOverridesFile)
	if err := os.WriteFile(yamlPath, []byte(legacy), 0644); err != nil {
		t.Fatal(err)
	}

	// 回退读取旧版文件
	overrides, err := LoadSchemaOverrides()
	if err != nil {
		t.Fatalf("LoadSchemaOverrides 失败: %v", err)
	}
	if _, ok := overrides["wubi86"]; !ok {
		t.Fatalf("应读到旧版覆盖, 实际=%v", overrides)
	}

	// 保存触发迁移
	if err := SetSchemaOverride("pinyin", map[string]any{"engine": map[string]any{"x": true}}); err != nil {
		t.Fatalf("SetSchemaOverride 失败: %v", err)
	}
	if _, err := os.Stat(filepath.Join(configDir, SchemaOverridesFile)); err != nil {
		t.Errorf("保存后 schema_overrides.toml 应存在: %v", err)
	}
	if _, err := os.Stat(yamlPath); !os.IsNotExist(err) {
		t.Error("保存后 schema_overrides.yaml 应已改名")
	}

	// 两个方案的覆盖都在
	overrides, err = LoadSchemaOverrides()
	if err != nil {
		t.Fatal(err)
	}
	if _, ok := overrides["wubi86"]; !ok {
		t.Errorf("迁移后旧覆盖丢失: %v", overrides)
	}
	if _, ok := overrides["pinyin"]; !ok {
		t.Errorf("迁移后新覆盖丢失: %v", overrides)
	}
}

// 删除最后一个覆盖时，残留的旧版 YAML 也应一并清理，防止"覆盖复活"。
func TestSchemaOverrides_DeleteCleansLegacyYAML(t *testing.T) {
	configDir := setTestConfigDir(t)
	if err := os.MkdirAll(configDir, 0755); err != nil {
		t.Fatal(err)
	}

	legacy := "wubi86:\n  learning:\n    temp_promote_count: 3\n"
	yamlPath := filepath.Join(configDir, LegacySchemaOverridesFile)
	if err := os.WriteFile(yamlPath, []byte(legacy), 0644); err != nil {
		t.Fatal(err)
	}

	if err := DeleteSchemaOverride("wubi86"); err != nil {
		t.Fatalf("DeleteSchemaOverride 失败: %v", err)
	}

	if _, err := os.Stat(yamlPath); !os.IsNotExist(err) {
		t.Error("删除后旧版 yaml 应已改名，否则覆盖会复活")
	}
	overrides, err := LoadSchemaOverrides()
	if err != nil {
		t.Fatal(err)
	}
	if len(overrides) != 0 {
		t.Errorf("删除后应无覆盖, 实际=%v", overrides)
	}
}
