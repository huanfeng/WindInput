package binformat

import (
	"os"
	"path/filepath"
	"testing"
)

// writeSharedTestWdb 写一个最小 wdb 文件供共享池测试使用。
func writeSharedTestWdb(t *testing.T) string {
	t.Helper()
	w := NewDictWriter()
	w.AddCode("ni", []DictEntry{{Text: "你", Weight: 100}})
	w.AddCode("hao", []DictEntry{{Text: "好", Weight: 90}})

	path := filepath.Join(t.TempDir(), "shared_test.wdb")
	f, err := os.Create(path)
	if err != nil {
		t.Fatalf("创建文件失败: %v", err)
	}
	if err := w.Write(f); err != nil {
		f.Close()
		t.Fatalf("写入失败: %v", err)
	}
	f.Close()
	return path
}

func TestOpenDictShared_SameInstance(t *testing.T) {
	path := writeSharedTestWdb(t)

	r1, err := OpenDict(path)
	if err != nil {
		t.Fatalf("第一次 OpenDict: %v", err)
	}
	r2, err := OpenDict(path)
	if err != nil {
		t.Fatalf("第二次 OpenDict: %v", err)
	}
	if r1 != r2 {
		t.Fatalf("同一文件两次 OpenDict 应返回同一实例: %p vs %p", r1, r2)
	}

	// 第一个持有者关闭后，reader 仍应可用（第二个持有者还在）
	if err := r1.Close(); err != nil {
		t.Fatalf("第一次 Close: %v", err)
	}
	if got := r2.Lookup("ni"); len(got) != 1 || got[0].Text != "你" {
		t.Fatalf("一个持有者 Close 后查询失效: %v", got)
	}

	// 最后一个持有者关闭后，reader 真正释放
	if err := r2.Close(); err != nil {
		t.Fatalf("第二次 Close: %v", err)
	}
	if !r2.isClosed() {
		t.Fatal("全部持有者 Close 后 reader 应已释放")
	}
	if got := r2.Lookup("ni"); len(got) != 0 {
		t.Fatalf("释放后查询应返回空: %v", got)
	}
}

func TestOpenDictShared_ReopenAfterClose(t *testing.T) {
	path := writeSharedTestWdb(t)

	r1, err := OpenDict(path)
	if err != nil {
		t.Fatalf("OpenDict: %v", err)
	}
	if err := r1.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}

	// 全部关闭后重开应得到新实例，且功能正常
	r2, err := OpenDict(path)
	if err != nil {
		t.Fatalf("重开 OpenDict: %v", err)
	}
	defer r2.Close()
	if r1 == r2 {
		t.Fatal("释放后重开应返回新实例")
	}
	if got := r2.Lookup("hao"); len(got) != 1 || got[0].Text != "好" {
		t.Fatalf("重开后查询失败: %v", got)
	}
}

func TestOpenDictShared_ForceCloseInterplay(t *testing.T) {
	path := writeSharedTestWdb(t)

	r1, err := OpenDict(path)
	if err != nil {
		t.Fatalf("OpenDict: %v", err)
	}
	r2, err := OpenDict(path)
	if err != nil {
		t.Fatalf("OpenDict: %v", err)
	}
	if r1 != r2 {
		t.Fatal("应为同一共享实例")
	}

	// 模拟词库重建前的强制关闭
	if n := CloseReadersForPath(path); n != 1 {
		t.Fatalf("CloseReadersForPath 应关闭 1 个底层 reader（共享只有一份），实际 %d", n)
	}
	if !r1.isClosed() {
		t.Fatal("强关后 reader 应已释放")
	}

	// 强关后重开应得到全新 reader（共享池已摘除失效条目）
	r3, err := OpenDict(path)
	if err != nil {
		t.Fatalf("强关后重开: %v", err)
	}
	if r3 == r1 {
		t.Fatal("强关后重开不应复用失效实例")
	}
	if got := r3.Lookup("ni"); len(got) != 1 {
		t.Fatalf("强关后重开查询失败: %v", got)
	}

	// 残余持有者的 Close 不应影响新 reader，也不应 panic
	if err := r1.Close(); err != nil {
		t.Fatalf("残余持有者 Close: %v", err)
	}
	if err := r2.Close(); err != nil {
		t.Fatalf("残余持有者 Close: %v", err)
	}
	if r3.isClosed() {
		t.Fatal("残余持有者 Close 不应波及新 reader")
	}
	if err := r3.Close(); err != nil {
		t.Fatalf("新 reader Close: %v", err)
	}
}

func TestOpenUnigramShared_SameInstance(t *testing.T) {
	w := NewUnigramWriter()
	w.Add("的", -2.5)
	w.Add("你好", -6.0)

	path := filepath.Join(t.TempDir(), "shared_unigram.wdb")
	f, err := os.Create(path)
	if err != nil {
		t.Fatalf("创建文件失败: %v", err)
	}
	if err := w.Write(f); err != nil {
		f.Close()
		t.Fatalf("写入失败: %v", err)
	}
	f.Close()

	r1, err := OpenUnigram(path)
	if err != nil {
		t.Fatalf("第一次 OpenUnigram: %v", err)
	}
	r2, err := OpenUnigram(path)
	if err != nil {
		t.Fatalf("第二次 OpenUnigram: %v", err)
	}
	if r1 != r2 {
		t.Fatal("同一文件两次 OpenUnigram 应返回同一实例")
	}

	if err := r1.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if !r2.Contains("你好") {
		t.Fatal("一个持有者 Close 后查询失效")
	}
	if err := r2.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if !r2.isClosed() {
		t.Fatal("全部持有者 Close 后应已释放")
	}
}
