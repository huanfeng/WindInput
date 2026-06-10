package config

import (
	"fmt"
	"reflect"
	"testing"
)

// fillValue 用反射递归填充 v 的所有导出字段：指针指向已填充的新值、
// slice 含 2 个已填充元素、map 含 1 个已填充键值对、标量取非零值。
// 目的：让别名检查覆盖配置树里**每一个**引用类型字段（含未来新增的）。
func fillValue(v reflect.Value, seed int) {
	switch v.Kind() {
	case reflect.Pointer:
		np := reflect.New(v.Type().Elem())
		fillValue(np.Elem(), seed+1)
		v.Set(np)
	case reflect.Slice:
		ns := reflect.MakeSlice(v.Type(), 2, 2)
		for i := 0; i < 2; i++ {
			fillValue(ns.Index(i), seed+i+1)
		}
		v.Set(ns)
	case reflect.Map:
		nm := reflect.MakeMap(v.Type())
		nk := reflect.New(v.Type().Key()).Elem()
		fillValue(nk, seed+1)
		nv := reflect.New(v.Type().Elem()).Elem()
		fillValue(nv, seed+2)
		nm.SetMapIndex(nk, nv)
		v.Set(nm)
	case reflect.Array:
		for i := 0; i < v.Len(); i++ {
			fillValue(v.Index(i), seed+i+1)
		}
	case reflect.Struct:
		for i := 0; i < v.NumField(); i++ {
			f := v.Field(i)
			if !f.CanSet() {
				continue
			}
			fillValue(f, seed+i+1)
		}
	case reflect.String:
		v.SetString(fmt.Sprintf("v%d", seed))
	case reflect.Bool:
		v.SetBool(true)
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
		v.SetInt(int64(seed))
	case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64:
		v.SetUint(uint64(seed))
	case reflect.Float32, reflect.Float64:
		v.SetFloat(float64(seed))
	}
}

// assertNoAliasing 递归断言 a 与 b 在 path 下不共享任何 map/slice/指针底层存储。
func assertNoAliasing(t *testing.T, path string, a, b reflect.Value) {
	t.Helper()
	switch a.Kind() {
	case reflect.Pointer:
		if a.IsNil() || b.IsNil() {
			if a.IsNil() != b.IsNil() {
				t.Errorf("%s: nil 不一致", path)
			}
			return
		}
		if a.Pointer() == b.Pointer() {
			t.Errorf("%s: 指针共享（浅拷贝泄漏）", path)
			return
		}
		assertNoAliasing(t, path+".*", a.Elem(), b.Elem())
	case reflect.Slice:
		if a.IsNil() {
			return
		}
		if a.Pointer() == b.Pointer() {
			t.Errorf("%s: slice 底层数组共享（浅拷贝泄漏）", path)
			return
		}
		for i := 0; i < a.Len() && i < b.Len(); i++ {
			assertNoAliasing(t, fmt.Sprintf("%s[%d]", path, i), a.Index(i), b.Index(i))
		}
	case reflect.Map:
		if a.IsNil() {
			return
		}
		if a.Pointer() == b.Pointer() {
			t.Errorf("%s: map 共享（浅拷贝泄漏，并发读写会 panic）", path)
			return
		}
		iter := a.MapRange()
		for iter.Next() {
			bv := b.MapIndex(iter.Key())
			if bv.IsValid() {
				assertNoAliasing(t, fmt.Sprintf("%s[%v]", path, iter.Key()), iter.Value(), bv)
			}
		}
	case reflect.Array:
		for i := 0; i < a.Len(); i++ {
			assertNoAliasing(t, fmt.Sprintf("%s[%d]", path, i), a.Index(i), b.Index(i))
		}
	case reflect.Struct:
		for i := 0; i < a.NumField(); i++ {
			ft := a.Type().Field(i)
			if !ft.IsExported() {
				// Clone 跳过未导出字段——配置树不应出现，出现即失败提醒维护者
				t.Errorf("%s.%s: Config 树存在未导出字段，Clone 无法拷贝", path, ft.Name)
				continue
			}
			assertNoAliasing(t, path+"."+ft.Name, a.Field(i), b.Field(i))
		}
	}
}

// TestClone_DeepEqualAndNoAliasing 防漂移守护：自动填充 Config 全部字段
// （含未来新增的引用类型字段），克隆后断言①值完全相等②零底层共享。
// 任何人新增 map/slice/指针配置字段，本测试自动覆盖，无需手工登记。
func TestClone_DeepEqualAndNoAliasing(t *testing.T) {
	orig := &Config{}
	fillValue(reflect.ValueOf(orig).Elem(), 1)

	cl := orig.Clone()

	if !reflect.DeepEqual(orig, cl) {
		t.Fatalf("Clone 与原件不相等")
	}
	assertNoAliasing(t, "Config", reflect.ValueOf(orig).Elem(), reflect.ValueOf(cl).Elem())
}

// TestClone_MutationIsolation 行为级验证：改原件的 map/slice 不影响克隆体。
func TestClone_MutationIsolation(t *testing.T) {
	orig := DefaultConfig()
	orig.Input.PunctCustom.Mappings = map[string][]string{"a": {"x"}}
	orig.Input.SpecialModes = []SpecialModeConfig{{ID: "m1", TriggerKeys: []string{"k"}}}
	orig.Hotkeys.ToggleModeKeys = []string{"shift"}

	cl := orig.Clone()

	orig.Input.PunctCustom.Mappings["a"][0] = "mutated"
	orig.Input.PunctCustom.Mappings["b"] = []string{"new"}
	orig.Input.SpecialModes[0].TriggerKeys[0] = "mutated"
	orig.Hotkeys.ToggleModeKeys[0] = "mutated"

	if cl.Input.PunctCustom.Mappings["a"][0] != "x" {
		t.Errorf("克隆体 Mappings 被原件修改污染")
	}
	if _, ok := cl.Input.PunctCustom.Mappings["b"]; ok {
		t.Errorf("克隆体 Mappings 出现原件新增的键")
	}
	if cl.Input.SpecialModes[0].TriggerKeys[0] != "k" {
		t.Errorf("克隆体 SpecialModes 被原件修改污染")
	}
	if cl.Hotkeys.ToggleModeKeys[0] != "shift" {
		t.Errorf("克隆体 ToggleModeKeys 被原件修改污染")
	}
}

func TestClone_Nil(t *testing.T) {
	var c *Config
	if c.Clone() != nil {
		t.Fatalf("nil Config 的 Clone 应返回 nil")
	}
}
