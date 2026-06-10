package datformat

import (
	"testing"

	"github.com/huanfeng/wind_input/internal/dict/binformat"
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

// TestOpenWdatShared_ForceCloseForRebuild 模拟词库重建：原子替换 wdat 前
// CloseReadersForPath 必须能强关共享 reader（绕过引用计数），否则 Windows 上
// rename 会因本进程 mmap 锁失败（实测回归：重建缓存后拼音生成失败）。
func TestOpenWdatShared_ForceCloseForRebuild(t *testing.T) {
	w := NewWdatWriter()
	w.AddCode("ni", []WdatEntry{{Text: "你", Weight: 100}})
	path := writeTestWdat(t, w)

	// 两个持有者共享同一 reader（refs=2），模拟多方案共用 pinyin.wdat
	r1, err := OpenWdat(path)
	if err != nil {
		t.Fatalf("OpenWdat: %v", err)
	}
	r2, err := OpenWdat(path)
	if err != nil {
		t.Fatalf("OpenWdat: %v", err)
	}
	if r1 != r2 {
		t.Fatal("应为同一共享实例")
	}

	// 强关必须无视引用计数，关闭唯一一份底层 mmap
	if n := binformat.CloseReadersForPath(path); n != 1 {
		t.Fatalf("CloseReadersForPath 应强关 1 个 reader，实际 %d", n)
	}
	if !r1.isClosed() {
		t.Fatal("强关后 reader 应已释放映射")
	}
	if got := r1.Lookup("ni"); len(got) != 0 {
		t.Fatalf("强关后查询应返回空: %v", got)
	}

	// 强关后重开应得到全新 reader，残余持有者 Close 不波及
	r3, err := OpenWdat(path)
	if err != nil {
		t.Fatalf("强关后重开: %v", err)
	}
	if r3 == r1 {
		t.Fatal("强关后重开不应复用失效实例")
	}
	if err := r1.Close(); err != nil {
		t.Fatalf("残余持有者 Close: %v", err)
	}
	if err := r2.Close(); err != nil {
		t.Fatalf("残余持有者 Close: %v", err)
	}
	if r3.isClosed() {
		t.Fatal("残余持有者 Close 不应波及新 reader")
	}
	if got := r3.Lookup("ni"); len(got) != 1 {
		t.Fatalf("新 reader 查询失败: %v", got)
	}
	if err := r3.Close(); err != nil {
		t.Fatalf("新 reader Close: %v", err)
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
