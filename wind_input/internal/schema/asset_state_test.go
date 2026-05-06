package schema

import (
	"errors"
	"fmt"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

// TestErrAssetBuilding_WrappedIsMatchable 验证用 %w 包装后，
// 上层可通过 errors.Is 区分"资源构建中"与其他错误。
func TestErrAssetBuilding_WrappedIsMatchable(t *testing.T) {
	wrapped := fmt.Errorf("%w: 拼音 wdat 词库正在后台生成", ErrAssetBuilding)
	if !errors.Is(wrapped, ErrAssetBuilding) {
		t.Fatal("expected errors.Is(wrapped, ErrAssetBuilding) == true")
	}

	plain := errors.New("some other failure")
	if errors.Is(plain, ErrAssetBuilding) {
		t.Fatal("plain error should not match ErrAssetBuilding")
	}
}

// TestOnPinyinWdatReady_ImmediateWhenIdle 验证"当前未构建时回调立即同步触发"。
func TestOnPinyinWdatReady_ImmediateWhenIdle(t *testing.T) {
	resetWdatStateForTest(t)

	called := false
	OnPinyinWdatReady(func() {
		called = true
	})
	if !called {
		t.Fatal("callback should have been called synchronously when not building")
	}
}

// TestOnPinyinWdatReady_QueuedWhenBuilding 验证"构建中注册的回调在构建结束时触发，
// 顺序与注册顺序一致"。这里通过手动操纵 building flag 模拟构建生命周期，
// 不真正跑 ConvertPinyinToWdat（依赖文件系统）。
func TestOnPinyinWdatReady_QueuedWhenBuilding(t *testing.T) {
	resetWdatStateForTest(t)

	// 手动进入"构建中"
	pinyinWdatBuildMu.Lock()
	pinyinWdatBuilding = true
	pinyinWdatBuildMu.Unlock()

	var order []int
	var mu sync.Mutex
	add := func(i int) func() {
		return func() {
			mu.Lock()
			order = append(order, i)
			mu.Unlock()
		}
	}

	OnPinyinWdatReady(add(1))
	OnPinyinWdatReady(add(2))
	OnPinyinWdatReady(add(3))

	// 仍在"构建中"，回调不应被调用
	mu.Lock()
	if len(order) != 0 {
		t.Fatalf("callbacks should be queued, got %v", order)
	}
	mu.Unlock()

	// 模拟"构建完成"——按生产代码相同的顺序：先取出 callbacks，
	// 改 flag，释放锁，然后逐个调用。
	pinyinWdatBuildMu.Lock()
	pinyinWdatBuilding = false
	cbs := pinyinWdatReadyCallbacks
	pinyinWdatReadyCallbacks = nil
	pinyinWdatBuildMu.Unlock()
	for _, cb := range cbs {
		cb()
	}

	mu.Lock()
	defer mu.Unlock()
	if len(order) != 3 || order[0] != 1 || order[1] != 2 || order[2] != 3 {
		t.Fatalf("expected callbacks in registration order [1 2 3], got %v", order)
	}
}

// TestIsPinyinWdatBuilding_Reflectsflag 简单回归：查询函数读取的就是当前 flag。
func TestIsPinyinWdatBuilding_Reflectsflag(t *testing.T) {
	resetWdatStateForTest(t)

	if IsPinyinWdatBuilding() {
		t.Fatal("expected false at start")
	}

	pinyinWdatBuildMu.Lock()
	pinyinWdatBuilding = true
	pinyinWdatBuildMu.Unlock()

	if !IsPinyinWdatBuilding() {
		t.Fatal("expected true while building")
	}

	pinyinWdatBuildMu.Lock()
	pinyinWdatBuilding = false
	pinyinWdatBuildMu.Unlock()
}

// TestOnPinyinWdatReady_ConcurrentRegistration 在构建中并发注册回调，
// 完成时所有回调都应被触发恰好一次。
func TestOnPinyinWdatReady_ConcurrentRegistration(t *testing.T) {
	resetWdatStateForTest(t)

	pinyinWdatBuildMu.Lock()
	pinyinWdatBuilding = true
	pinyinWdatBuildMu.Unlock()

	const N = 50
	var counter atomic.Int32
	var wg sync.WaitGroup
	wg.Add(N)
	for i := 0; i < N; i++ {
		go func() {
			defer wg.Done()
			OnPinyinWdatReady(func() { counter.Add(1) })
		}()
	}
	wg.Wait()

	// 触发完成
	pinyinWdatBuildMu.Lock()
	pinyinWdatBuilding = false
	cbs := pinyinWdatReadyCallbacks
	pinyinWdatReadyCallbacks = nil
	pinyinWdatBuildMu.Unlock()
	for _, cb := range cbs {
		cb()
	}

	// 给可能的 immediate 路径一点收敛时间（实际此测试里不应有 immediate）
	time.Sleep(10 * time.Millisecond)

	if got := counter.Load(); got != N {
		t.Fatalf("expected %d callbacks fired exactly once, got %d", N, got)
	}
}

// resetWdatStateForTest 把全局状态恢复到干净起点。
// 使用 t.Cleanup 注册再次清理，避免测试间互相污染。
func resetWdatStateForTest(t *testing.T) {
	t.Helper()
	pinyinWdatBuildMu.Lock()
	pinyinWdatBuilding = false
	pinyinWdatReadyCallbacks = nil
	pinyinWdatBuildMu.Unlock()
	t.Cleanup(func() {
		pinyinWdatBuildMu.Lock()
		pinyinWdatBuilding = false
		pinyinWdatReadyCallbacks = nil
		pinyinWdatBuildMu.Unlock()
	})
}
