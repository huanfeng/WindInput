package config

import (
	"os"
	"path/filepath"
	"testing"
)

// setTestConfigDir 临时将 APPDATA 指向临时目录，使 GetConfigDir() 返回临时路径
// 返回 restore 函数，defer 调用即可恢复
func setTestConfigDir(t *testing.T) string {
	t.Helper()
	tmpDir := t.TempDir()
	orig := os.Getenv("APPDATA")
	os.Setenv("APPDATA", tmpDir)
	t.Cleanup(func() {
		os.Setenv("APPDATA", orig)
	})
	return tmpDir
}

func TestSchemaOverrides_LoadEmpty(t *testing.T) {
	setTestConfigDir(t)

	overrides, err := LoadSchemaOverrides()
	if err != nil {
		t.Fatalf("文件不存在时不应返回错误: %v", err)
	}
	if len(overrides) != 0 {
		t.Fatalf("文件不存在时应返回空 map, 实际=%v", overrides)
	}
}

func TestSchemaOverrides_SaveAndLoad(t *testing.T) {
	setTestConfigDir(t)

	input := map[string]map[string]any{
		"wubi86": {
			"engine": map[string]any{
				"codetable": map[string]any{
					"auto_commit_unique": true,
				},
			},
		},
		"pinyin": {
			"engine": map[string]any{
				"pinyin": map[string]any{
					"show_code_hint": false,
				},
			},
		},
	}

	if err := SaveSchemaOverrides(input); err != nil {
		t.Fatalf("SaveSchemaOverrides 失败: %v", err)
	}

	loaded, err := LoadSchemaOverrides()
	if err != nil {
		t.Fatalf("LoadSchemaOverrides 失败: %v", err)
	}

	if len(loaded) != 2 {
		t.Fatalf("期望 2 个方案, 实际=%d", len(loaded))
	}

	if _, ok := loaded["wubi86"]; !ok {
		t.Error("wubi86 方案应存在")
	}
	if _, ok := loaded["pinyin"]; !ok {
		t.Error("pinyin 方案应存在")
	}
}

func TestSchemaOverrides_SetAndGet(t *testing.T) {
	setTestConfigDir(t)

	override := map[string]any{
		"learning": map[string]any{
			"freq": map[string]any{
				"enabled": true,
			},
		},
	}

	if err := SetSchemaOverride("wubi86", override); err != nil {
		t.Fatalf("SetSchemaOverride 失败: %v", err)
	}

	got, err := GetSchemaOverride("wubi86")
	if err != nil {
		t.Fatalf("GetSchemaOverride 失败: %v", err)
	}
	if got == nil {
		t.Fatal("期望获取到覆盖配置, 实际为 nil")
	}

	learning, ok := got["learning"].(map[string]any)
	if !ok {
		t.Fatalf("learning 应为 map, 实际=%T", got["learning"])
	}
	freq, ok := learning["freq"].(map[string]any)
	if !ok {
		t.Fatalf("freq 应为 map, 实际=%T", learning["freq"])
	}
	if freq["enabled"] != true {
		t.Errorf("期望 enabled=true, 实际=%v", freq["enabled"])
	}
}

func TestSchemaOverrides_GetNonExistent(t *testing.T) {
	setTestConfigDir(t)

	got, err := GetSchemaOverride("nonexistent")
	if err != nil {
		t.Fatalf("GetSchemaOverride 不存在方案时不应报错: %v", err)
	}
	if got != nil {
		t.Fatalf("不存在的方案应返回 nil, 实际=%v", got)
	}
}

func TestSchemaOverrides_Delete(t *testing.T) {
	setTestConfigDir(t)

	// 先设置两个方案
	if err := SetSchemaOverride("wubi86", map[string]any{"foo": "bar"}); err != nil {
		t.Fatalf("SetSchemaOverride 失败: %v", err)
	}
	if err := SetSchemaOverride("pinyin", map[string]any{"baz": "qux"}); err != nil {
		t.Fatalf("SetSchemaOverride 失败: %v", err)
	}

	// 删除 wubi86
	if err := DeleteSchemaOverride("wubi86"); err != nil {
		t.Fatalf("DeleteSchemaOverride 失败: %v", err)
	}

	got, err := GetSchemaOverride("wubi86")
	if err != nil {
		t.Fatalf("GetSchemaOverride 失败: %v", err)
	}
	if got != nil {
		t.Fatalf("删除后应返回 nil, 实际=%v", got)
	}

	// pinyin 仍应存在
	pinyin, err := GetSchemaOverride("pinyin")
	if err != nil {
		t.Fatalf("GetSchemaOverride 失败: %v", err)
	}
	if pinyin == nil {
		t.Fatal("pinyin 方案仍应存在")
	}
}

func TestSchemaOverrides_DeleteLastCleansFile(t *testing.T) {
	tmpDir := setTestConfigDir(t)

	// 先设置一个方案
	if err := SetSchemaOverride("wubi86", map[string]any{"foo": "bar"}); err != nil {
		t.Fatalf("SetSchemaOverride 失败: %v", err)
	}

	// 确认文件存在
	appName := "WindInput"
	overridesPath := filepath.Join(tmpDir, appName, SchemaOverridesFile)
	if _, err := os.Stat(overridesPath); os.IsNotExist(err) {
		t.Fatal("保存后文件应存在")
	}

	// 删除最后一个方案
	if err := DeleteSchemaOverride("wubi86"); err != nil {
		t.Fatalf("DeleteSchemaOverride 失败: %v", err)
	}

	// 文件应被删除
	if _, err := os.Stat(overridesPath); !os.IsNotExist(err) {
		t.Fatal("删除最后一个方案后文件应被删除")
	}
}
