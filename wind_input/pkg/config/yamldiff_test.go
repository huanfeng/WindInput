package config

import (
	"testing"
)

func TestComputeYAMLDiff_ScalarChanges(t *testing.T) {
	base := map[string]interface{}{
		"name":  "test",
		"value": 10,
		"flag":  true,
	}
	current := map[string]interface{}{
		"name":  "test", // 相同
		"value": 20,     // 不同
		"flag":  true,   // 相同
	}

	diff, err := ComputeYAMLDiff(base, current)
	if err != nil {
		t.Fatal(err)
	}

	if len(diff) != 1 {
		t.Fatalf("期望 1 个差异, 实际=%d, diff=%v", len(diff), diff)
	}
	if diff["value"] != 20 {
		t.Errorf("期望 value=20, 实际=%v", diff["value"])
	}
}

func TestComputeYAMLDiff_NestedMap(t *testing.T) {
	base := map[string]interface{}{
		"engine": map[string]interface{}{
			"type":        "codetable",
			"filter_mode": "smart",
			"codetable": map[string]interface{}{
				"max_code_length":    4,
				"auto_commit_unique": false,
				"top_code_commit":    true,
			},
		},
	}
	current := map[string]interface{}{
		"engine": map[string]interface{}{
			"type":        "codetable", // 相同
			"filter_mode": "smart",     // 相同
			"codetable": map[string]interface{}{
				"max_code_length":    4,    // 相同
				"auto_commit_unique": true, // 不同
				"top_code_commit":    true, // 相同
			},
		},
	}

	diff, err := ComputeYAMLDiff(base, current)
	if err != nil {
		t.Fatal(err)
	}

	engine, ok := diff["engine"].(map[string]interface{})
	if !ok {
		t.Fatalf("engine 应为 map, 实际=%T", diff["engine"])
	}

	// engine 层只应保留 codetable（type 和 filter_mode 相同被过滤）
	if _, exists := engine["type"]; exists {
		t.Error("type 未变化不应出现在 diff 中")
	}

	codetable, ok := engine["codetable"].(map[string]interface{})
	if !ok {
		t.Fatalf("codetable 应为 map, 实际=%T", engine["codetable"])
	}

	if len(codetable) != 1 {
		t.Fatalf("codetable 应只有 1 个差异, 实际=%d, diff=%v", len(codetable), codetable)
	}
	if codetable["auto_commit_unique"] != true {
		t.Errorf("auto_commit_unique 应为 true")
	}
}

func TestComputeYAMLDiff_SliceChanged(t *testing.T) {
	base := map[string]interface{}{
		"items": []interface{}{"a", "b"},
	}
	current := map[string]interface{}{
		"items": []interface{}{"a", "b", "c"},
	}

	diff, err := ComputeYAMLDiff(base, current)
	if err != nil {
		t.Fatal(err)
	}

	items, ok := diff["items"].([]interface{})
	if !ok {
		t.Fatalf("items 应为切片, 实际=%T", diff["items"])
	}
	if len(items) != 3 {
		t.Errorf("items 应完整保留, 长度=%d", len(items))
	}
}

func TestComputeYAMLDiff_SliceUnchanged(t *testing.T) {
	base := map[string]interface{}{
		"items": []interface{}{"a", "b"},
	}
	current := map[string]interface{}{
		"items": []interface{}{"a", "b"},
	}

	diff, err := ComputeYAMLDiff(base, current)
	if err != nil {
		t.Fatal(err)
	}

	if len(diff) != 0 {
		t.Errorf("无差异时应为空 map, 实际=%v", diff)
	}
}

func TestComputeYAMLDiff_NewField(t *testing.T) {
	base := map[string]interface{}{
		"name": "test",
	}
	current := map[string]interface{}{
		"name":      "test",
		"new_field": "new_value",
	}

	diff, err := ComputeYAMLDiff(base, current)
	if err != nil {
		t.Fatal(err)
	}

	if diff["new_field"] != "new_value" {
		t.Errorf("新增字段应保留, diff=%v", diff)
	}
	if _, exists := diff["name"]; exists {
		t.Error("未变化字段不应出现")
	}
}

func TestComputeYAMLDiff_NoDiff(t *testing.T) {
	base := map[string]interface{}{
		"a": 1,
		"b": "hello",
		"c": true,
	}
	current := map[string]interface{}{
		"a": 1,
		"b": "hello",
		"c": true,
	}

	diff, err := ComputeYAMLDiff(base, current)
	if err != nil {
		t.Fatal(err)
	}

	if len(diff) != 0 {
		t.Errorf("完全相同时 diff 应为空, 实际=%v", diff)
	}
}

func TestComputeYAMLDiff_BoolFalseVsTrue(t *testing.T) {
	// 确保 bool false 不被忽略（对比 base 的 true）
	base := map[string]interface{}{
		"enabled": true,
	}
	current := map[string]interface{}{
		"enabled": false,
	}

	diff, err := ComputeYAMLDiff(base, current)
	if err != nil {
		t.Fatal(err)
	}

	if diff["enabled"] != false {
		t.Errorf("bool false 应作为差异保留, diff=%v", diff)
	}
}

func TestComputeYAMLDiff_ZeroValueVsNonZero(t *testing.T) {
	// 确保数值 0 对比非 0 时保留
	base := map[string]interface{}{
		"count": 3,
	}
	current := map[string]interface{}{
		"count": 0,
	}

	diff, err := ComputeYAMLDiff(base, current)
	if err != nil {
		t.Fatal(err)
	}

	if diff["count"] != 0 {
		t.Errorf("数值 0 应作为差异保留, diff=%v", diff)
	}
}

func TestComputeYAMLDiff_WithStructs(t *testing.T) {
	// 测试使用实际 struct 类型（通过 YAML 序列化后比较）
	type Inner struct {
		Value int    `yaml:"value"`
		Label string `yaml:"label"`
	}
	type Outer struct {
		Name  string `yaml:"name"`
		Inner Inner  `yaml:"inner"`
	}

	base := Outer{Name: "test", Inner: Inner{Value: 10, Label: "old"}}
	current := Outer{Name: "test", Inner: Inner{Value: 20, Label: "old"}}

	diff, err := ComputeYAMLDiff(base, current)
	if err != nil {
		t.Fatal(err)
	}

	inner, ok := diff["inner"].(map[string]interface{})
	if !ok {
		t.Fatalf("inner 应为 map, diff=%v", diff)
	}
	if inner["value"] != 20 {
		t.Errorf("inner.value 应为 20, 实际=%v", inner["value"])
	}
	if _, exists := inner["label"]; exists {
		t.Error("inner.label 未变化不应出现")
	}
	if _, exists := diff["name"]; exists {
		t.Error("name 未变化不应出现")
	}
}

func TestComputeYAMLDiff_RemovedFieldNotInDiff(t *testing.T) {
	// base 有但 current 没有的字段不应出现在 diff 中
	base := map[string]interface{}{
		"keep":   1,
		"remove": 2,
	}
	current := map[string]interface{}{
		"keep": 1,
	}

	diff, err := ComputeYAMLDiff(base, current)
	if err != nil {
		t.Fatal(err)
	}

	if len(diff) != 0 {
		t.Errorf("无新增或修改时 diff 应为空, 实际=%v", diff)
	}
}
