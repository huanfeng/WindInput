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
	if s.Learning.IsAutoLearnEnabled() {
		t.Error("码表方案默认不应启用自动造词")
	}
	if s.Learning.IsFreqEnabled() {
		t.Error("码表方案默认不应启用调频")
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
    show_code_hint: false
    use_smart_compose: true
  filter_mode: smart
dictionaries:
  - id: main
    path: "dict/pinyin"
    type: rime_pinyin
    default: true
user_data:
  shadow_file: "shuangpin.shadow.yaml"
  user_dict_file: "shuangpin.userwords.txt"
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
	// 拼音方案默认启用自动造词和调频
	if !s.Learning.IsAutoLearnEnabled() {
		t.Error("拼音方案默认应启用自动造词")
	}
	if !s.Learning.IsFreqEnabled() {
		t.Error("拼音方案默认应启用调频")
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

func TestDiscoverSchemas_MergeUserWithBuiltin(t *testing.T) {
	exeDir := t.TempDir()
	dataDir := t.TempDir()

	// 内置方案：完整的 codetable 方案，带 encoder 和 learning 配置
	exeSchemaDir := filepath.Join(exeDir, "schemas")
	os.MkdirAll(exeSchemaDir, 0755)
	builtinContent := `
schema:
  id: test_merge
  name: "测试方案"
  icon_label: "测"
  version: "1.0"
engine:
  type: codetable
  codetable:
    max_code_length: 4
    auto_commit_unique: false
    top_code_commit: true
    show_code_hint: true
    candidate_sort_mode: frequency
  filter_mode: smart
dictionaries:
  - id: main
    path: "dict/test.txt"
    type: rime_codetable
    default: true
learning:
  freq:
    protect_top_n: 3
encoder:
  max_word_length: 10
  rules:
    - length_equal: 2
      formula: "AaAbBaBb"
`
	os.WriteFile(filepath.Join(exeSchemaDir, "test_merge.schema.yaml"), []byte(builtinContent), 0644)

	// 用户方案：只修改部分字段（auto_commit_unique 和 freq.protect_top_n）
	userSchemaDir := filepath.Join(dataDir, "schemas")
	os.MkdirAll(userSchemaDir, 0755)
	userContent := `
schema:
  id: test_merge
engine:
  codetable:
    auto_commit_unique: true
learning:
  freq:
    protect_top_n: 5
`
	os.WriteFile(filepath.Join(userSchemaDir, "test_merge.schema.yaml"), []byte(userContent), 0644)

	schemas, err := DiscoverSchemas(exeDir, dataDir)
	if err != nil {
		t.Fatalf("DiscoverSchemas 失败: %v", err)
	}

	s := schemas["test_merge"]
	if s == nil {
		t.Fatal("test_merge 方案不存在")
	}

	// 用户修改的字段应生效
	if s.Engine.CodeTable == nil {
		t.Fatal("codetable 配置不应为 nil")
	}
	if !s.Engine.CodeTable.AutoCommitUnique {
		t.Error("auto_commit_unique 应被用户覆盖为 true")
	}
	if s.Learning.Freq == nil || s.Learning.Freq.ProtectTopN != 5 {
		protectN := 0
		if s.Learning.Freq != nil {
			protectN = s.Learning.Freq.ProtectTopN
		}
		t.Errorf("freq.protect_top_n 应被用户覆盖为 5, 实际=%d", protectN)
	}

	// 内置默认值应保留
	if s.Engine.Type != EngineTypeCodeTable {
		t.Errorf("engine.type 应保留内置值 codetable, 实际=%s", s.Engine.Type)
	}
	if s.Engine.CodeTable.MaxCodeLength != 4 {
		t.Errorf("max_code_length 应保留内置值 4, 实际=%d", s.Engine.CodeTable.MaxCodeLength)
	}
	if !s.Engine.CodeTable.TopCodeCommit {
		t.Error("top_code_commit 应保留内置值 true")
	}
	if !s.Engine.CodeTable.ShowCodeHint {
		t.Error("show_code_hint 应保留内置值 true")
	}
	if s.Engine.FilterMode != "smart" {
		t.Errorf("filter_mode 应保留内置值 smart, 实际=%s", s.Engine.FilterMode)
	}
	if s.Schema.Name != "测试方案" {
		t.Errorf("name 应保留内置值, 实际=%s", s.Schema.Name)
	}
	if s.Encoder == nil {
		t.Error("encoder 应保留内置配置")
	} else if s.Encoder.MaxWordLength != 10 {
		t.Errorf("encoder.max_word_length 应保留内置值 10, 实际=%d", s.Encoder.MaxWordLength)
	}

	// dictionaries 应保留内置值（用户未指定）
	if len(s.Dicts) != 1 || s.Dicts[0].ID != "main" {
		t.Errorf("dictionaries 应保留内置配置, 实际=%d 个", len(s.Dicts))
	}
}

func TestSchemaManager(t *testing.T) {
	exeDir := t.TempDir()
	dataDir := t.TempDir()

	exeSchemaDir := filepath.Join(exeDir, "schemas")
	os.MkdirAll(exeSchemaDir, 0755)
	writeTestSchema(t, exeSchemaDir, "wubi86.schema.yaml", "wubi86", "codetable")
	writeTestSchema(t, exeSchemaDir, "pinyin.schema.yaml", "pinyin", "pinyin")

	sm := NewSchemaManager(exeDir, dataDir, nil)
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
  shadow_file: "` + id + `.shadow.yaml"
  user_dict_file: "` + id + `.userwords.txt"
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

// TestAutoCommitAtFull_BackwardCompat 验证旧 YAML 只设置 auto_commit_unique=true 时，
// 加载后 CodeTableSpec.AutoCommitUnique 为 true 且 AutoCommitAtFull 为 nil（让 factory 回退）。
func TestAutoCommitAtFull_BackwardCompat(t *testing.T) {
	tmpDir := t.TempDir()
	content := `
schema:
  id: test_compat
  name: "兼容测试"
  version: "1.0"
engine:
  type: codetable
  codetable:
    max_code_length: 4
    auto_commit_unique: true
  filter_mode: smart
dictionaries:
  - id: main
    path: "dict/x.txt"
    type: codetable
    default: true
learning:
  mode: manual
`
	path := filepath.Join(tmpDir, "compat.schema.yaml")
	if err := os.WriteFile(path, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
	s, err := LoadSchemaFile(path)
	if err != nil {
		t.Fatalf("LoadSchemaFile: %v", err)
	}
	spec := s.Engine.CodeTable
	if spec == nil {
		t.Fatalf("CodeTable spec 为 nil")
	}
	if !spec.AutoCommitUnique {
		t.Errorf("AutoCommitUnique 应为 true")
	}
	if spec.AutoCommitAtFull != nil {
		t.Errorf("AutoCommitAtFull 应为 nil（未设置），实际 %v", *spec.AutoCommitAtFull)
	}
	// 模拟 factory 回退逻辑
	effective := func() bool {
		if spec.AutoCommitAtFull != nil {
			return *spec.AutoCommitAtFull
		}
		return spec.AutoCommitUnique
	}()
	if !effective {
		t.Errorf("回退后 AutoCommitAtFull 等价值应为 true")
	}
}

func TestDictSpec_IsEnabled(t *testing.T) {
	boolPtr := func(b bool) *bool { return &b }

	tests := []struct {
		name           string
		defaultEnabled *bool
		enabled        *bool
		want           bool
	}{
		{"两者均 nil → 默认 true", nil, nil, true},
		{"defaultEnabled=false, enabled=nil → false", boolPtr(false), nil, false},
		{"defaultEnabled=false, enabled=true → true（用户覆盖）", boolPtr(false), boolPtr(true), true},
		{"defaultEnabled=true, enabled=false → false（用户覆盖）", boolPtr(true), boolPtr(false), false},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			d := DictSpec{DefaultEnabled: tc.defaultEnabled, Enabled: tc.enabled}
			if got := d.IsEnabled(); got != tc.want {
				t.Errorf("IsEnabled()=%v, want %v", got, tc.want)
			}
		})
	}
}

func TestDictSpec_DisplayLabel(t *testing.T) {
	d1 := DictSpec{ID: "wubi86_main", Label: "极点主词库"}
	if d1.DisplayLabel() != "极点主词库" {
		t.Errorf("有 Label 时应返回 Label, got %s", d1.DisplayLabel())
	}
	d2 := DictSpec{ID: "wubi86_extra"}
	if d2.DisplayLabel() != "wubi86_extra" {
		t.Errorf("无 Label 时应回退为 id, got %s", d2.DisplayLabel())
	}
}

func TestMergeDictsByID(t *testing.T) {
	boolPtr := func(b bool) *bool { return &b }

	base := []DictSpec{
		{ID: "main", Path: "main.yaml", Type: "rime_codetable", Default: true},
		{ID: "extra", Path: "extra.yaml", Type: "rime_codetable", DefaultEnabled: boolPtr(false)},
	}

	t.Run("用户只覆盖 enabled 字段", func(t *testing.T) {
		overrides := []DictSpec{
			{ID: "extra", Enabled: boolPtr(true)},
		}
		result := mergeDictsByID(base, overrides)
		if len(result) != 2 {
			t.Fatalf("期望 2 条，实际 %d", len(result))
		}
		if result[1].Path != "extra.yaml" {
			t.Errorf("Path 应从 base 继承，got %s", result[1].Path)
		}
		if result[1].Enabled == nil || !*result[1].Enabled {
			t.Error("Enabled 应被设为 true")
		}
	})

	t.Run("用户新增第三方词库", func(t *testing.T) {
		overrides := []DictSpec{
			{ID: "custom", Path: "custom.yaml", Type: "rime_codetable"},
		}
		result := mergeDictsByID(base, overrides)
		if len(result) != 3 {
			t.Fatalf("期望 3 条，实际 %d", len(result))
		}
		if result[2].ID != "custom" {
			t.Errorf("新增词库应追加到末尾，got %s", result[2].ID)
		}
	})

	t.Run("用户新增词库缺少 Path 或 Type 时不追加", func(t *testing.T) {
		overrides := []DictSpec{
			{ID: "bad", Path: "", Type: "rime_codetable"},
		}
		result := mergeDictsByID(base, overrides)
		if len(result) != 2 {
			t.Fatalf("不完整的新增词库不应追加，实际 %d", len(result))
		}
	})

	t.Run("overrides 为空时返回 base 副本", func(t *testing.T) {
		result := mergeDictsByID(base, nil)
		if len(result) != len(base) {
			t.Fatalf("期望 %d，实际 %d", len(base), len(result))
		}
	})
}
