package config

import (
	"errors"
	"fmt"
)

// currentConfigVersion 当前配置结构版本。
// 版本边界（设计：docs/design/config-restructure.md §4.1，格式即版本边界）：
//   - v0 = 旧版 YAML 结构（YAML 文件永不携带 version 键，格式本身即判据）
//   - v1 = TOML 新结构（顶层裸键 version = 1）
//
// Config struct 不含 version 字段：version 只存活于磁盘文件与 map 层
// （驱动迁移循环、SaveTo 注入），unmarshal 进 struct 时被自然丢弃。
const currentConfigVersion = 1

// ErrFutureConfigVersion 配置文件版本高于当前程序支持的版本（用户回滚了旧版程序）。
// 调用方按"文件损坏"分支处理：备份 .bak + 回退默认 + 警告，不尝试解读未来格式。
var ErrFutureConfigVersion = errors.New("config version is newer than supported")

// configMigration 单步迁移函数：把 map 层配置从版本 N 就地升级到 N+1。
type configMigration func(m map[string]any)

// configMigrations 版本迁移链，key = 起始版本。
var configMigrations = map[int]configMigration{
	0: migrateV0toV1,
}

// migrateConfigMap 把 map 层配置就地迁移到 currentConfigVersion。
//
// fromLegacyYAML=true 表示来源是旧版 .yaml 回退文件（恒为 v0）；否则按 map 中
// version 键判定，键缺失时按当前版本处理——v0 只可能是 YAML，TOML 缺 version
// 必然是用户手编误删，不执行任何迁移，下次 diff-save 自动补写（设计 §4.1 规则 2）。
//
// 返回 migrated=true 表示至少执行了一步迁移（调用方应触发写出持久化）。
func migrateConfigMap(m map[string]any, fromLegacyYAML bool) (migrated bool, err error) {
	v := currentConfigVersion
	if fromLegacyYAML {
		v = 0
	} else if n, ok := safeGetInt(m, "version"); ok {
		v = n
	}
	if v > currentConfigVersion {
		return false, fmt.Errorf("%w: file=%d supported=%d", ErrFutureConfigVersion, v, currentConfigVersion)
	}
	for ; v < currentConfigVersion; v++ {
		mig, ok := configMigrations[v]
		if !ok {
			return migrated, fmt.Errorf("missing config migration for version %d", v)
		}
		mig(m)
		migrated = true
	}
	m["version"] = currentConfigVersion
	return migrated, nil
}

// injectVersion 把当前版本号写进待序列化的配置 map（SaveTo 的 diff/全量两分支用）。
// version 是元数据不参与 diff，故在 diff 计算之后强制注入；空 diff 也至少写出
// version 一行，作为迁移完成标记。
func injectVersion(m map[string]any) {
	m["version"] = currentConfigVersion
}

// ---- map 安全取值辅助 ----
//
// 迁移函数禁止裸类型断言（m["x"].(bool) 类型不符即 panic）：统一走 safeGet*，
// 断言失败返回零值 + false，调用方按"该键缺失"处理——用户手编脏数据降级为
// 丢弃该项，不崩启动。数值做 int/int64/uint64/float64 宽容归一（v0 输入恒来自
// yaml.v3，类型面单一为 int，但归一防未来迁移函数在 TOML 来源 map 上复用）。

// safeGetMap 取嵌套子表。
func safeGetMap(m map[string]any, key string) (map[string]any, bool) {
	v, ok := m[key]
	if !ok {
		return nil, false
	}
	sub, ok := v.(map[string]any)
	return sub, ok
}

// safeGetString 取字符串值。
func safeGetString(m map[string]any, key string) (string, bool) {
	v, ok := m[key]
	if !ok {
		return "", false
	}
	s, ok := v.(string)
	return s, ok
}

// safeGetBool 取布尔值。
func safeGetBool(m map[string]any, key string) (bool, bool) {
	v, ok := m[key]
	if !ok {
		return false, false
	}
	b, ok := v.(bool)
	return b, ok
}

// safeGetInt 取整数值（int/int64/uint64 直收，float64 仅在无小数部分时收）。
func safeGetInt(m map[string]any, key string) (int, bool) {
	v, ok := m[key]
	if !ok {
		return 0, false
	}
	switch n := v.(type) {
	case int:
		return n, true
	case int64:
		return int(n), true
	case uint64:
		return int(n), true
	case float64:
		if n == float64(int(n)) {
			return int(n), true
		}
	}
	return 0, false
}

// safeGetSlice 取数组值。
func safeGetSlice(m map[string]any, key string) ([]any, bool) {
	v, ok := m[key]
	if !ok {
		return nil, false
	}
	s, ok := v.([]any)
	return s, ok
}
