package schema

import (
	"os"
	"path/filepath"
	"testing"
)

func TestLoadSchemaFile(t *testing.T) {
	// 创建临时方案文件
	tmpDir := t.TempDir()
	schemaContent := `
schema:
  id: test_wubi
  name: "测试五笔"
  icon_label: "测"
  version: "1.0"
engine:
  type: codetable
  codetable:
    max_code_length: 4
    top_code_commit: true
  filter_mode: smart
dictionaries:
  - id: main
    path: "dict/test.txt"
    type: codetable
    default: true
user_data:
  shadow_file: "shadow_test.yaml"
  user_dict_file: "user_words_test.txt"
learning:
  mode: manual
`
	path := filepath.Join(tmpDir, "test.schema.yaml")
	if err := os.WriteFile(path, []byte(schemaContent), 0644); err != nil {
		t.Fatal(err)
	}

	s, err := LoadSchemaFile(path)
	if err != nil {
		t.Fatalf("LoadSchemaFile 失败: %v", err)
	}

	if s.Schema.ID != "test_wubi" {
		t.Errorf("期望 ID=test_wubi, 实际=%s", s.Schema.ID)
	}
	if s.Schema.Name != "测试五笔" {
		t.Errorf("期望 Name=测试五笔, 实际=%s", s.Schema.Name)
	}
	if s.Engine.Type != EngineTypeCodeTable {
		t.Errorf("期望 Type=codetable, 实际=%s", s.Engine.Type)
	}
	if s.Engine.CodeTable == nil {
		t.Fatal("CodeTable 配置不应为 nil")
	}
	if s.Engine.CodeTable.MaxCodeLength != 4 {
		t.Errorf("期望 MaxCodeLength=4, 实际=%d", s.Engine.CodeTable.MaxCodeLength)
	}
	if s.Engine.CodeTable.TopCodeCommit != true {
		t.Error("期望 TopCodeCommit=true")
	}
	if len(s.Dicts) != 1 {
		t.Fatalf("期望 1 个词库, 实际=%d", len(s.Dicts))
	}
	if s.Dicts[0].Type != "codetable" {
		t.Errorf("期望词库类型=codetable, 实际=%s", s.Dicts[0].Type)
	}
	if s.Learning.Mode != LearningManual {
		t.Errorf("期望 LearningMode=manual, 实际=%s", s.Learning.Mode)
	}
}

func TestLoadSchemaFile_PinyinWithShuangpin(t *testing.T) {
	tmpDir := t.TempDir()
	schemaContent := `
schema:
  id: shuangpin_zrm
  name: "双拼自然码"
engine:
  type: pinyin
  pinyin:
    scheme: shuangpin
    shuangpin:
      layout: ziranma
    show_wubi_hint: false
    use_smart_compose: true
  filter_mode: smart
dictionaries:
  - id: main
    path: "dict/pinyin"
    type: rime_pinyin
    default: true
user_data:
  shadow_file: "shadow_shuangpin.yaml"
  user_dict_file: "user_words_shuangpin.txt"
`
	path := filepath.Join(tmpDir, "sp.schema.yaml")
	os.WriteFile(path, []byte(schemaContent), 0644)

	s, err := LoadSchemaFile(path)
	if err != nil {
		t.Fatalf("LoadSchemaFile 失败: %v", err)
	}

	if s.Engine.Type != EngineTypePinyin {
		t.Errorf("期望 Type=pinyin, 实际=%s", s.Engine.Type)
	}
	if s.Engine.Pinyin == nil {
		t.Fatal("Pinyin 配置不应为 nil")
	}
	if s.Engine.Pinyin.Scheme != "shuangpin" {
		t.Errorf("期望 Scheme=shuangpin, 实际=%s", s.Engine.Pinyin.Scheme)
	}
	if s.Engine.Pinyin.Shuangpin == nil {
		t.Fatal("Shuangpin 配置不应为 nil")
	}
	if s.Engine.Pinyin.Shuangpin.Layout != "ziranma" {
		t.Errorf("期望 Layout=ziranma, 实际=%s", s.Engine.Pinyin.Shuangpin.Layout)
	}
	// 默认学习模式应为 auto（pinyin 类型）
	if s.Learning.Mode != LearningAuto {
		t.Errorf("期望 LearningMode=auto, 实际=%s", s.Learning.Mode)
	}
}

func TestValidateSchema_MissingFields(t *testing.T) {
	tests := []struct {
		name    string
		content string
		wantErr string
	}{
		{
			name:    "缺少 schema.id",
			content: `engine: {type: codetable}`,
			wantErr: "schema.id 不能为空",
		},
		{
			name: "缺少 engine.type",
			content: `
schema: {id: test}
dictionaries: [{path: "x", type: "y"}]
user_data: {shadow_file: "s.yaml", user_dict_file: "u.txt"}`,
			wantErr: "engine.type 不能为空",
		},
		{
			name: "不支持的 engine.type",
			content: `
schema: {id: test}
engine: {type: unknown}
dictionaries: [{path: "x", type: "y"}]
user_data: {shadow_file: "s.yaml", user_dict_file: "u.txt"}`,
			wantErr: "不支持的值",
		},
		{
			name: "缺少 dictionaries",
			content: `
schema: {id: test}
engine: {type: codetable}
user_data: {shadow_file: "s.yaml", user_dict_file: "u.txt"}`,
			wantErr: "dictionaries 不能为空",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tmpDir := t.TempDir()
			path := filepath.Join(tmpDir, "bad.schema.yaml")
			os.WriteFile(path, []byte(tt.content), 0644)

			_, err := LoadSchemaFile(path)
			if err == nil {
				t.Fatal("期望出错但成功了")
			}
			if !containsStr(err.Error(), tt.wantErr) {
				t.Errorf("期望错误包含 %q, 实际: %v", tt.wantErr, err)
			}
		})
	}
}

func TestDiscoverSchemas(t *testing.T) {
	exeDir := t.TempDir()
	dataDir := t.TempDir()

	// 内置方案
	exeSchemaDir := filepath.Join(exeDir, "schemas")
	os.MkdirAll(exeSchemaDir, 0755)
	writeTestSchema(t, exeSchemaDir, "builtin.schema.yaml", "builtin", "codetable")

	// 用户方案（同 ID 覆盖）
	userSchemaDir := filepath.Join(dataDir, "schemas")
	os.MkdirAll(userSchemaDir, 0755)
	writeTestSchema(t, userSchemaDir, "builtin.schema.yaml", "builtin", "pinyin")
	writeTestSchema(t, userSchemaDir, "custom.schema.yaml", "custom", "codetable")

	schemas, err := DiscoverSchemas(exeDir, dataDir)
	if err != nil {
		t.Fatalf("DiscoverSchemas 失败: %v", err)
	}

	if len(schemas) != 2 {
		t.Fatalf("期望 2 个方案, 实际=%d", len(schemas))
	}

	// builtin 应被用户版本覆盖（pinyin）
	if schemas["builtin"].Engine.Type != EngineTypePinyin {
		t.Errorf("builtin 方案应被覆盖为 pinyin, 实际=%s", schemas["builtin"].Engine.Type)
	}

	if _, ok := schemas["custom"]; !ok {
		t.Error("custom 方案应存在")
	}
}

func TestSchemaManager(t *testing.T) {
	exeDir := t.TempDir()
	dataDir := t.TempDir()

	exeSchemaDir := filepath.Join(exeDir, "schemas")
	os.MkdirAll(exeSchemaDir, 0755)
	writeTestSchema(t, exeSchemaDir, "wubi86.schema.yaml", "wubi86", "codetable")
	writeTestSchema(t, exeSchemaDir, "pinyin.schema.yaml", "pinyin", "pinyin")

	sm := NewSchemaManager(exeDir, dataDir)
	if err := sm.LoadSchemas(); err != nil {
		t.Fatalf("LoadSchemas 失败: %v", err)
	}

	if sm.SchemaCount() != 2 {
		t.Fatalf("期望 2 个方案, 实际=%d", sm.SchemaCount())
	}

	if err := sm.SetActive("wubi86"); err != nil {
		t.Fatalf("SetActive 失败: %v", err)
	}
	if sm.GetActiveID() != "wubi86" {
		t.Errorf("期望 activeID=wubi86, 实际=%s", sm.GetActiveID())
	}

	s := sm.GetActiveSchema()
	if s == nil || s.Schema.ID != "wubi86" {
		t.Error("GetActiveSchema 返回错误")
	}

	if err := sm.SetActive("nonexistent"); err == nil {
		t.Error("SetActive 不存在的方案应返回错误")
	}
}

func TestGetDefaultDictSpec(t *testing.T) {
	s := &Schema{
		Dicts: []DictSpec{
			{ID: "a", Path: "a.txt", Type: "codetable", Default: false},
			{ID: "b", Path: "b.txt", Type: "codetable", Default: true},
		},
	}
	d := s.GetDefaultDictSpec()
	if d == nil || d.ID != "b" {
		t.Error("应返回 default=true 的词库")
	}

	s2 := &Schema{
		Dicts: []DictSpec{
			{ID: "a", Path: "a.txt", Type: "codetable"},
		},
	}
	d2 := s2.GetDefaultDictSpec()
	if d2 == nil || d2.ID != "a" {
		t.Error("无 default 时应返回第一个")
	}
}

// --- helpers ---

func writeTestSchema(t *testing.T, dir, filename, id string, engineType EngineType) {
	t.Helper()
	dictType := "codetable"
	if engineType == EngineTypePinyin {
		dictType = "rime_pinyin"
	}
	content := `
schema:
  id: ` + id + `
  name: "` + id + `"
engine:
  type: ` + string(engineType) + `
dictionaries:
  - id: main
    path: "dict/test.txt"
    type: ` + dictType + `
    default: true
user_data:
  shadow_file: "shadow_` + id + `.yaml"
  user_dict_file: "user_words_` + id + `.txt"
`
	path := filepath.Join(dir, filename)
	if err := os.WriteFile(path, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
}

func containsStr(s, sub string) bool {
	return len(s) >= len(sub) && (s == sub || len(s) > 0 && containsSubstr(s, sub))
}

func containsSubstr(s, sub string) bool {
	for i := 0; i <= len(s)-len(sub); i++ {
		if s[i:i+len(sub)] == sub {
			return true
		}
	}
	return false
}
