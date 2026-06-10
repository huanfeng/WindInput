package config

import "reflect"

// Clone 返回 Config 的深拷贝。
//
// 用途：异步持久化（go config.Save(...)）前必须深拷贝——`cfgCopy := *cfg` 浅拷贝
// 会与在用配置共享底层 map/slice/指针（如 Input.PunctCustom.Mappings、
// Input.SpecialModes、Hotkeys.ToggleModeKeys），后台 yaml 序列化遍历这些 map 时
// 若前台并发修改，将触发 concurrent map iteration and map write 硬 panic。
// 因此所有 `go config.Save` 一律先 Clone，禁止浅拷贝快照。
//
// 实现为反射式通用深拷贝：自动覆盖全部导出字段（含嵌套 struct/slice/map/指针），
// 新增配置字段无需同步修改本文件；正确性由 clone_test.go 的反射别名检查守护
// （自动填充所有引用字段后断言克隆体与原件零共享）。
func (c *Config) Clone() *Config {
	if c == nil {
		return nil
	}
	out := &Config{}
	deepCopyValue(reflect.ValueOf(out).Elem(), reflect.ValueOf(c).Elem())
	return out
}

// deepCopyValue 把 src 深拷贝进 dst（二者类型一致、dst 可写）。
// 覆盖配置树会出现的全部种类；未导出字段跳过（Config 树应保持全导出，
// clone_test.go 会在出现未导出字段时失败提醒）。
func deepCopyValue(dst, src reflect.Value) {
	switch src.Kind() {
	case reflect.Pointer:
		if src.IsNil() {
			return
		}
		np := reflect.New(src.Type().Elem())
		deepCopyValue(np.Elem(), src.Elem())
		dst.Set(np)
	case reflect.Slice:
		if src.IsNil() {
			return
		}
		ns := reflect.MakeSlice(src.Type(), src.Len(), src.Len())
		for i := 0; i < src.Len(); i++ {
			deepCopyValue(ns.Index(i), src.Index(i))
		}
		dst.Set(ns)
	case reflect.Map:
		if src.IsNil() {
			return
		}
		nm := reflect.MakeMapWithSize(src.Type(), src.Len())
		iter := src.MapRange()
		for iter.Next() {
			nk := reflect.New(src.Type().Key()).Elem()
			deepCopyValue(nk, iter.Key())
			nv := reflect.New(src.Type().Elem()).Elem()
			deepCopyValue(nv, iter.Value())
			nm.SetMapIndex(nk, nv)
		}
		dst.Set(nm)
	case reflect.Array:
		for i := 0; i < src.Len(); i++ {
			deepCopyValue(dst.Index(i), src.Index(i))
		}
	case reflect.Struct:
		for i := 0; i < src.NumField(); i++ {
			if !dst.Field(i).CanSet() {
				continue // 未导出字段：跳过（由测试守护不出现）
			}
			deepCopyValue(dst.Field(i), src.Field(i))
		}
	default:
		dst.Set(src)
	}
}
