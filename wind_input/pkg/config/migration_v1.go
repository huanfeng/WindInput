package config

// migrateV0toV1 把 v0（旧版 YAML 结构）配置 map 就地迁移为 v1 新结构。
// 完整 key 映射表见 docs/design/config-restructure.md §6；
// 同时熔合 v0 时代的四个启发式迁移（quick_input 旧字段、theme:"dark"、
// status_indicator 旧顶层字段回填、font_size_follow_theme 老用户语义）。
//
// 注意：本函数操作的 map 恒来自 yaml.v3 解析（v0 必为 YAML），
// 取值一律走 safeGet*（migrate.go），脏数据按键缺失降级。
func migrateV0toV1(m map[string]any) {
	// TODO(切片 1)：§6 映射表搬键 + 启发式熔合。
	// 切片 0 仅占位以保证迁移链表完整；LoadFrom 接线同在切片 1。
}
