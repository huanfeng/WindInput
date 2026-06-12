package schema

import (
	"os"
	"path/filepath"
	"reflect"
	"testing"
)

// 同一方案分别用 YAML 与 TOML 手写，加载后应得到深度相等的 Schema。
// 验证：双 tag 一致、go-toml/v2 原生解码、嵌套表 / 数组表 / 指针三态字段的等价。
const equivYAML = `
schema:
  id: test_wb
  name: "测试"
  icon_label: "测"
engine:
  type: codetable
  codetable:
    max_code_length: 4
    top_code_commit: true
    candidate_sort_mode: frequency
    temp_pinyin:
      enabled: true
      schema: pinyin
  filter_mode: smart
dictionaries:
  - id: main
    label: "主"
    path: "a.dict.toml"
    type: rime_codetable
    default: true
  - id: extra
    path: "b.dict.toml"
    type: rime_codetable
    default_enabled: true
    weight_as_order: true
  - id: off
    path: "c.dict.toml"
    type: rime_codetable
    enabled: false
learning:
  freq:
    enabled: true
    protect_top_n: 1
encoder:
  max_word_length: 10
  rules:
    - length_equal: 2
      formula: "AaAbBaBb"
    - length_in_range: [4, 10]
      formula: "AaBaCaZa"
`

const equivTOML = `
[schema]
id = "test_wb"
name = "测试"
icon_label = "测"

[engine]
type = "codetable"
filter_mode = "smart"

[engine.codetable]
max_code_length = 4
top_code_commit = true
candidate_sort_mode = "frequency"

[engine.codetable.temp_pinyin]
enabled = true
schema = "pinyin"

[[dictionaries]]
id = "main"
label = "主"
path = "a.dict.toml"
type = "rime_codetable"
default = true

[[dictionaries]]
id = "extra"
path = "b.dict.toml"
type = "rime_codetable"
default_enabled = true
weight_as_order = true

[[dictionaries]]
id = "off"
path = "c.dict.toml"
type = "rime_codetable"
enabled = false

[learning]
[learning.freq]
enabled = true
protect_top_n = 1

[encoder]
max_word_length = 10

[[encoder.rules]]
length_equal = 2
formula = "AaAbBaBb"

[[encoder.rules]]
length_in_range = [4, 10]
formula = "AaBaCaZa"
`

func TestSchemaTOMLYAMLEquivalence(t *testing.T) {
	dir := t.TempDir()
	yamlPath := filepath.Join(dir, "wb_y.schema.yaml")
	tomlPath := filepath.Join(dir, "wb_t.schema.toml")
	if err := os.WriteFile(yamlPath, []byte(equivYAML), 0644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(tomlPath, []byte(equivTOML), 0644); err != nil {
		t.Fatal(err)
	}

	sy, err := LoadSchemaFile(yamlPath)
	if err != nil {
		t.Fatalf("加载 yaml 失败: %v", err)
	}
	st, err := LoadSchemaFile(tomlPath)
	if err != nil {
		t.Fatalf("加载 toml 失败: %v", err)
	}

	// id 不同（文件名只是测试用），归一后比较其余全部字段
	sy.Schema.ID = "x"
	st.Schema.ID = "x"
	if !reflect.DeepEqual(sy, st) {
		t.Fatalf("toml 与 yaml 加载结果不一致:\nYAML=%+v\nTOML=%+v", sy, st)
	}

	// 抽样断言关键字段确实被解码（防止两边都解析为空导致假相等）
	if st.Engine.CodeTable == nil || st.Engine.CodeTable.MaxCodeLength != 4 {
		t.Fatalf("toml codetable 解码异常: %+v", st.Engine.CodeTable)
	}
	if st.Engine.CodeTable.TempPinyin == nil || !st.Engine.CodeTable.TempPinyin.Enabled {
		t.Fatalf("toml temp_pinyin 解码异常: %+v", st.Engine.CodeTable.TempPinyin)
	}
	if len(st.Dicts) != 3 {
		t.Fatalf("toml dictionaries 数量异常: %d", len(st.Dicts))
	}
	if st.Dicts[2].Enabled == nil || *st.Dicts[2].Enabled {
		t.Fatalf("toml enabled 指针三态解码异常: %+v", st.Dicts[2].Enabled)
	}
	if len(st.Encoder.Rules) != 2 || st.Encoder.Rules[1].LengthInRange[1] != 10 {
		t.Fatalf("toml encoder.rules 解码异常: %+v", st.Encoder)
	}
}

// discoverSchemaPaths：同 stem 同时存在 toml 与 yaml 时，仅返回 toml。
func TestDiscoverSchemaTOMLPriority(t *testing.T) {
	dir := t.TempDir()
	mustWrite(t, filepath.Join(dir, "foo.schema.yaml"), "schema:\n  id: foo_yaml\n")
	mustWrite(t, filepath.Join(dir, "foo.schema.toml"), "[schema]\nid = \"foo_toml\"\n")
	mustWrite(t, filepath.Join(dir, "bar.schema.yaml"), "schema:\n  id: bar_yaml\n")

	paths, err := discoverSchemaPaths(dir)
	if err != nil {
		t.Fatal(err)
	}
	if len(paths) != 2 {
		t.Fatalf("期望 2 个去重后路径，得到 %d: %v", len(paths), paths)
	}
	var hasFooTOML, hasFooYAML, hasBarYAML bool
	for _, p := range paths {
		switch filepath.Base(p) {
		case "foo.schema.toml":
			hasFooTOML = true
		case "foo.schema.yaml":
			hasFooYAML = true
		case "bar.schema.yaml":
			hasBarYAML = true
		}
	}
	if !hasFooTOML || hasFooYAML {
		t.Fatalf("foo 应只选 toml：toml=%v yaml=%v", hasFooTOML, hasFooYAML)
	}
	if !hasBarYAML {
		t.Fatalf("bar 仅有 yaml，应被选中")
	}
}

// 用户层 .schema.toml 部分覆盖内置层 .schema.yaml：
// 验证 go-toml/v2 解码进已填充 struct 的部分覆盖（保留内置缺失字段）+ 词库按 id 合并。
func TestUserSchemaTOMLOverridesBuiltinYAML(t *testing.T) {
	exeDir := t.TempDir()
	dataDir := t.TempDir()
	mustWrite(t, filepath.Join(exeDir, "schemas", "wb.schema.yaml"), equivYAML)
	// 用户 toml：改 name（标量覆盖）+ 关闭 extra 词库（按 id 合并，仅改 Enabled）
	userTOML := `
[schema]
id = "test_wb"
name = "用户改名"

[[dictionaries]]
id = "extra"
enabled = false
`
	mustWrite(t, filepath.Join(dataDir, "schemas", "wb.schema.toml"), userTOML)

	schemas, err := DiscoverSchemas(exeDir, dataDir)
	if err != nil {
		t.Fatal(err)
	}
	s := schemas["test_wb"]
	if s == nil {
		t.Fatal("未加载 test_wb 方案")
	}
	// 标量覆盖生效
	if s.Schema.Name != "用户改名" {
		t.Fatalf("name 覆盖未生效: %q", s.Schema.Name)
	}
	// 内置缺失字段保留（icon_label 仅内置 yaml 有，用户 toml 未写）
	if s.Schema.IconLabel != "测" {
		t.Fatalf("内置字段未保留: icon_label=%q", s.Schema.IconLabel)
	}
	// 内置嵌套结构保留（codetable 用户 toml 完全未提及）
	if s.Engine.CodeTable == nil || s.Engine.CodeTable.MaxCodeLength != 4 {
		t.Fatalf("内置嵌套 codetable 未保留: %+v", s.Engine.CodeTable)
	}
	// 词库按 id 合并：3 个内置词库保留，extra 被关闭
	if len(s.Dicts) != 3 {
		t.Fatalf("词库数量异常（应保留 3 个内置）: %d", len(s.Dicts))
	}
	var extra *DictSpec
	for i := range s.Dicts {
		if s.Dicts[i].ID == "extra" {
			extra = &s.Dicts[i]
		}
	}
	if extra == nil {
		t.Fatal("extra 词库丢失")
	}
	if extra.Enabled == nil || *extra.Enabled {
		t.Fatalf("extra.enabled 覆盖未生效: %+v", extra.Enabled)
	}
	// extra 的内置字段（path/weight_as_order）应保留
	if extra.Path != "b.dict.toml" || !extra.WeightAsOrder {
		t.Fatalf("extra 内置字段未保留: %+v", extra)
	}
}

func mustWrite(t *testing.T, path, content string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
}
