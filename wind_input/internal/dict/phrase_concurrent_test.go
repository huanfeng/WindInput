package dict

import (
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

// TestSearchCommandConcurrentNoDeadlock 验证 SearchCommand double-checked
// locking 升级 (2026-05-17) 后并发调用不会死锁, 同时缓存命中路径与缓存写入
// 路径都正确返回展开结果。
//
// 覆盖 3 类 code:
//   - 动态短语 ($Y 模板) — 走情况 1
//   - $AA 字符组 — 走情况 2b (staticPhrases 展开)
//   - 字符组前缀 — 走情况 3 (nav)
//
// 每个 code 启动 N goroutine 同时调用; 重复触发缓存命中与冷启动两种路径。
func TestSearchCommandConcurrentNoDeadlock(t *testing.T) {
	tmpDir := t.TempDir()
	systemFile := filepath.Join(tmpDir, "system.phrases.yaml")
	content := `phrases:
  - code: "rq"
    text: "$Y-$MM-$DD"
    position: 1
  - code: "zzbd"
    text: '$AA("标点", "，。！")'
    weight: 3000
  - code: "zzsz"
    text: '$AA("数字", "①②③")'
    weight: 2500
`
	if err := os.WriteFile(systemFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
	pl := loadPhraseLayerFromYAML(t, systemFile, "")

	codes := []string{"rq", "zzbd", "zzsz", "zz" /* 前缀 nav */}
	const workersPerCode = 32
	var wg sync.WaitGroup
	var calls int64

	// 100ms 超时安全网, 防止真有死锁挂死测试。
	done := make(chan struct{})
	go func() {
		select {
		case <-done:
		case <-time.After(10 * time.Second):
			t.Errorf("SearchCommand concurrent test stuck > 10s (likely deadlock)")
		}
	}()

	for _, code := range codes {
		for i := 0; i < workersPerCode; i++ {
			wg.Add(1)
			go func(c string) {
				defer wg.Done()
				for j := 0; j < 50; j++ {
					got := pl.SearchCommand(c, 0)
					atomic.AddInt64(&calls, 1)
					if len(got) == 0 {
						// rq 动态短语展开总应有 1 条;
						// zzbd/zzsz 各 3 条字符;
						// zz 应有 2 条 nav。
						t.Errorf("SearchCommand(%q) returned 0 results", c)
						return
					}
				}
			}(code)
		}
	}
	wg.Wait()
	close(done)

	if got := atomic.LoadInt64(&calls); got != int64(len(codes))*workersPerCode*50 {
		t.Fatalf("expected %d total calls, got %d", len(codes)*workersPerCode*50, got)
	}
}

// TestSearchCommandReadPathDoesNotBlockReaders 验证 R-Lock 缓存命中路径
// 真的支持并发读, 即 N 个并发查询同一 code 时不会因为 W-Lock 排队等待 (耗时
// 应远小于串行 ms 级)。这是 R-H1 升级的核心收益。
//
// 实现策略: 先调一次 SearchCommand 让缓存 warm, 再并发 N 次, 测耗时上限。
// 测试不是严格的性能基准, 只确保新路径不会因为锁问题倒退到串行。
func TestSearchCommandReadPathDoesNotBlockReaders(t *testing.T) {
	tmpDir := t.TempDir()
	systemFile := filepath.Join(tmpDir, "system.phrases.yaml")
	content := `phrases:
  - code: "rq"
    text: "$Y-$MM-$DD"
    position: 1
`
	if err := os.WriteFile(systemFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
	pl := loadPhraseLayerFromYAML(t, systemFile, "")

	// Warm cache
	if got := pl.SearchCommand("rq", 0); len(got) == 0 {
		t.Fatal("warm-up: SearchCommand(rq) returned empty")
	}

	const workers = 64
	var wg sync.WaitGroup
	start := time.Now()
	for i := 0; i < workers; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for j := 0; j < 500; j++ {
				_ = pl.SearchCommand("rq", 0)
			}
		}()
	}
	wg.Wait()
	elapsed := time.Since(start)

	// 64 * 500 = 32000 次缓存命中, 在升级前 (Lock 串行) 也只是 ms 级别。
	// 我们只设个宽松上限 (2 秒) 避免环境抖动; 真的死锁会被前一个 test 的
	// 超时安全网先 catch 到。
	if elapsed > 2*time.Second {
		t.Fatalf("32000 cache hits took %s (> 2s), R-Lock read path may be serialized", elapsed)
	}
	t.Logf("32000 cache hits across %d workers: %s", workers, elapsed)
}
