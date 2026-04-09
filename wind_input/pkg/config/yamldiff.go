package config

import (
	"reflect"

	"gopkg.in/yaml.v3"
)

// ComputeYAMLDiff 计算两个对象的 YAML 差异
// 返回一个 map，只包含 current 相对于 base 发生变化的字段
// 用于实现 diff 保存：只将用户修改过的字段写入用户配置文件
func ComputeYAMLDiff(base, current interface{}) (map[string]interface{}, error) {
	baseMap, err := toYAMLMap(base)
	if err != nil {
		return nil, err
	}

	currentMap, err := toYAMLMap(current)
	if err != nil {
		return nil, err
	}

	diff := diffMaps(baseMap, currentMap)
	return diff, nil
}

// toYAMLMap 通过 YAML 序列化/反序列化将任意对象转为 map
func toYAMLMap(v interface{}) (map[string]interface{}, error) {
	// 如果已经是 map 类型，直接做 YAML 往返确保类型一致
	data, err := yaml.Marshal(v)
	if err != nil {
		return nil, err
	}

	var m map[string]interface{}
	if err := yaml.Unmarshal(data, &m); err != nil {
		return nil, err
	}

	if m == nil {
		m = make(map[string]interface{})
	}
	return m, nil
}

// diffMaps 递归对比两个 map，返回只包含差异的 map
// - 标量值不同 → 保留 current 值
// - 嵌套 map → 递归对比，只保留有差异的子字段
// - 切片/数组 → 整体对比，不同则整体保留
// - base 中没有而 current 有 → 保留 current 值
// - base 中有而 current 没有 → 不输出（表示用户未设置，使用默认值）
func diffMaps(base, current map[string]interface{}) map[string]interface{} {
	diff := make(map[string]interface{})

	for key, curVal := range current {
		baseVal, exists := base[key]
		if !exists {
			// base 中不存在的字段，保留 current 值
			diff[key] = curVal
			continue
		}

		// 两者都是 map → 递归对比
		curMap, curIsMap := toMap(curVal)
		baseMap, baseIsMap := toMap(baseVal)
		if curIsMap && baseIsMap {
			subDiff := diffMaps(baseMap, curMap)
			if len(subDiff) > 0 {
				diff[key] = subDiff
			}
			continue
		}

		// 非 map 类型：深度比较
		if !deepEqual(baseVal, curVal) {
			diff[key] = curVal
		}
	}

	return diff
}

// toMap 尝试将值转为 map[string]interface{}
func toMap(v interface{}) (map[string]interface{}, bool) {
	if v == nil {
		return nil, false
	}
	if m, ok := v.(map[string]interface{}); ok {
		return m, true
	}
	return nil, false
}

// deepEqual 深度比较两个值是否相等
func deepEqual(a, b interface{}) bool {
	return reflect.DeepEqual(a, b)
}
