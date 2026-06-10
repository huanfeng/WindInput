package config

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// ---- migrateConfigMap 版本判别（设计 §4.1 格式即版本边界）----

// 旧版 YAML 来源恒为 v0：执行迁移链并标记 migrated。
func TestMigrateConfigMap_LegacyYAMLIsV0(t *testing.T) {
	m := map[string]any{"ui": map[string]any{"font_size": 20}}
	migrated, err := migrateConfigMap(m, true)
	if err != nil {
		t.Fatalf("migrateConfigMap 失败: %v", err)
	}
	if !migrated {
		t.Error("legacy yaml 来源应触发迁移")
	}
	if v, ok := safeGetInt(m, "version"); !ok || v != currentConfigVersion {
		t.Errorf("迁移后 version = %v, want %d", m["version"], currentConfigVersion)
	}
}

// TOML 缺 version = 按当前版本处理：不迁移（手编误删保护，设计 §4.1 规则 2）。
func TestMigrateConfigMap_TOMLMissingVersionIsCurrent(t *testing.T) {
	m := map[string]any{"general": map[string]any{"remember_last_state": true}}
	migrated, err := migrateConfigMap(m, false)
	if err != nil {
		t.Fatalf("migrateConfigMap 失败: %v", err)
	}
	if migrated {
		t.Error("TOML 缺 version 不应执行任何迁移")
	}
}

// 显式 version = currentConfigVersion：不迁移。
func TestMigrateConfigMap_ExplicitCurrent(t *testing.T) {
	m := map[string]any{"version": currentConfigVersion}
	migrated, err := migrateConfigMap(m, false)
	if err != nil {
		t.Fatalf("migrateConfigMap 失败: %v", err)
	}
	if migrated {
		t.Error("version=current 不应迁移")
	}
}

// 未来版本（程序回滚场景）：返回 ErrFutureConfigVersion，调用方走损坏分支。
func TestMigrateConfigMap_FutureVersion(t *testing.T) {
	m := map[string]any{"version": currentConfigVersion + 1}
	_, err := migrateConfigMap(m, false)
	if !errors.Is(err, ErrFutureConfigVersion) {
		t.Fatalf("未来版本应返回 ErrFutureConfigVersion, got %v", err)
	}
}

// ---- SaveTo 的 version 注入 ----

// diff 保存：version 必为文件首个键（锁定 go-toml v2 顶层标量先于表的行为，
// 换 marshal 库需重验——设计 §4.1）。
func TestSaveTo_VersionIsFirstKey(t *testing.T) {
	setTestConfigDir(t)
	dir := t.TempDir()
	path := filepath.Join(dir, "config.toml")

	cfg := SystemDefaultConfig()
	cfg.UI.FontSize = 33
	if err := SaveTo(cfg, path); err != nil {
		t.Fatalf("SaveTo 失败: %v", err)
	}

	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	firstLine := ""
	for _, line := range strings.Split(string(data), "\n") {
		if s := strings.TrimSpace(line); s != "" && !strings.HasPrefix(s, "#") {
			firstLine = s
			break
		}
	}
	if !strings.HasPrefix(firstLine, "version = 1") {
		t.Errorf("文件首个键应为 version = 1, 实际首行: %q\n全文:\n%s", firstLine, data)
	}
}

// 空 diff（与系统默认一致）也写出 version（迁移完成标记，设计 §4.3）。
func TestSaveTo_EmptyDiffWritesVersion(t *testing.T) {
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
	if !strings.Contains(string(data), "version = 1") {
		t.Errorf("空 diff 也应写出 version, 实际:\n%s", data)
	}
	// 除 version 外不应有其他键
	if strings.Contains(string(data), "[") {
		t.Errorf("空 diff 不应含任何 section, 实际:\n%s", data)
	}
	// 回读正常
	if _, err := LoadFrom(path); err != nil {
		t.Fatalf("仅含 version 的文件回读失败: %v", err)
	}
}

// ---- safeGet* 弱类型防护 ----

func TestSafeGetInt_Coercion(t *testing.T) {
	m := map[string]any{
		"i": int(3), "i64": int64(4), "u64": uint64(5),
		"f_whole": float64(6), "f_frac": float64(6.5),
		"s": "7", "b": true,
	}
	cases := []struct {
		key  string
		want int
		ok   bool
	}{
		{"i", 3, true}, {"i64", 4, true}, {"u64", 5, true},
		{"f_whole", 6, true}, {"f_frac", 0, false},
		{"s", 0, false}, {"b", 0, false}, {"missing", 0, false},
	}
	for _, c := range cases {
		got, ok := safeGetInt(m, c.key)
		if got != c.want || ok != c.ok {
			t.Errorf("safeGetInt(%q) = (%d,%v), want (%d,%v)", c.key, got, ok, c.want, c.ok)
		}
	}
}

func TestSafeGet_TypeMismatchNoPanic(t *testing.T) {
	m := map[string]any{
		"str_in_bool": "yes",
		"int_in_map":  42,
		"map_in_str":  map[string]any{"x": 1},
		"bool_in_arr": true,
	}
	if _, ok := safeGetBool(m, "str_in_bool"); ok {
		t.Error("字符串放 bool 位应返回 ok=false")
	}
	if _, ok := safeGetMap(m, "int_in_map"); ok {
		t.Error("int 放 map 位应返回 ok=false")
	}
	if _, ok := safeGetString(m, "map_in_str"); ok {
		t.Error("map 放 string 位应返回 ok=false")
	}
	if _, ok := safeGetSlice(m, "bool_in_arr"); ok {
		t.Error("bool 放 slice 位应返回 ok=false")
	}
}
