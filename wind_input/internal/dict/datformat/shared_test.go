package datformat

import (
	"testing"
)

func TestOpenWdatShared_SameInstance(t *testing.T) {
	w := NewWdatWriter()
	w.AddCode("ni", []WdatEntry{{Text: "你", Weight: 100}})
	w.AddCode("hao", []WdatEntry{{Text: "好", Weight: 90}})
	path := writeTestWdat(t, w)

	r1, err := OpenWdat(path)
	if err != nil {
		t.Fatalf("第一次 OpenWdat: %v", err)
	}
	r2, err := OpenWdat(path)
	if err != nil {
		t.Fatalf("第二次 OpenWdat: %v", err)
	}
	if r1 != r2 {
		t.Fatalf("同一文件两次 OpenWdat 应返回同一实例: %p vs %p", r1, r2)
	}

	// 第一个持有者关闭后，reader 仍应可用
	if err := r1.Close(); err != nil {
		t.Fatalf("第一次 Close: %v", err)
	}
	if got := r2.Lookup("ni"); len(got) != 1 || got[0].Text != "你" {
		t.Fatalf("一个持有者 Close 后查询失效: %v", got)
	}

	// 最后一个持有者关闭后真正释放；重复 Close 幂等不 panic
	if err := r2.Close(); err != nil {
		t.Fatalf("第二次 Close: %v", err)
	}
	if err := r2.Close(); err != nil {
		t.Fatalf("重复 Close 应幂等: %v", err)
	}
}

func TestOpenWdatShared_ReopenAfterClose(t *testing.T) {
	w := NewWdatWriter()
	w.AddCode("san", []WdatEntry{{Text: "三", Weight: 150}})
	path := writeTestWdat(t, w)

	r1, err := OpenWdat(path)
	if err != nil {
		t.Fatalf("OpenWdat: %v", err)
	}
	if err := r1.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}

	r2, err := OpenWdat(path)
	if err != nil {
		t.Fatalf("重开 OpenWdat: %v", err)
	}
	defer r2.Close()
	if r1 == r2 {
		t.Fatal("释放后重开应返回新实例")
	}
	if got := r2.Lookup("san"); len(got) != 1 || got[0].Text != "三" {
		t.Fatalf("重开后查询失败: %v", got)
	}
}
