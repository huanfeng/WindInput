// lifecycle_mu_test.go — 验证 HandleIMEActivated 在 push 期间不持有 c.mu
//
// 回归场景：
//   HandleIMEActivated（快路径，不含 200ms 超时保护）曾在 c.mu 持有期间
//   调用 PushEnglishPairConfigToAllClients，而该调用内部做阻塞式 named pipe
//   写入。若对端缓冲区满，c.mu 会被长时间占用，导致所有依赖 c.mu 的慢路径
//   命令（ShowContextMenu / ToggleMode / IMEDeactivated）全部超时。
//
// 修复：在锁内读取所有字段后立即解锁，push 调用移到锁外。
// 本测试用慢 push mock 验证解锁时机：push 进行中时，c.mu 应可被立即获取。
package coordinator

import (
	"log/slog"
	"sync"
	"testing"
	"time"

	"github.com/huanfeng/wind_input/pkg/config"
)

// slowPushBridgeServer 包装 mockBridgeServer，覆盖 PushEnglishPairConfigToAllClients
// 使其阻塞 pushDelay，并在开始阻塞前关闭 pushStarted channel 通知测试。
type slowPushBridgeServer struct {
	mockBridgeServer
	pushDelay   time.Duration
	pushStarted chan struct{}
	once        sync.Once
}

func (s *slowPushBridgeServer) PushEnglishPairConfigToAllClients(_ bool, _ []string) {
	s.once.Do(func() { close(s.pushStarted) }) // 通知测试：push 已开始
	time.Sleep(s.pushDelay)
}

// TestHandleIMEActivated_MuReleasedBeforePush 验证 HandleIMEActivated 在 push
// 进行期间已释放 c.mu，其他 goroutine 不会被阻塞。
func TestHandleIMEActivated_MuReleasedBeforePush(t *testing.T) {
	const pushDelay = 100 * time.Millisecond
	const maxLockWait = 10 * time.Millisecond

	pushStarted := make(chan struct{})
	slow := &slowPushBridgeServer{
		pushDelay:   pushDelay,
		pushStarted: pushStarted,
	}

	cfg := &config.Config{}
	c := &Coordinator{
		logger:       slog.New(slog.DiscardHandler),
		config:       cfg,
		cfgMu:        new(sync.RWMutex),
		bridgeServer: slow,
		chineseMode:  true,
	}

	// 在后台调用 HandleIMEActivated；processID=0 跳过 Win32 进程名查询
	done := make(chan struct{})
	go func() {
		defer close(done)
		c.HandleIMEActivated(0)
	}()

	// 等待 push 开始（此时按修复后的逻辑，c.mu 应已解锁）
	select {
	case <-pushStarted:
	case <-time.After(500 * time.Millisecond):
		t.Fatal("PushEnglishPairConfigToAllClients 未在超时内被调用，检查 bridgeServer/config 初始化")
	}

	// push 仍在阻塞中，验证 c.mu 可以被立即获取
	lockStart := time.Now()
	c.mu.Lock()
	lockWait := time.Since(lockStart)
	c.mu.Unlock()

	if lockWait > maxLockWait {
		t.Errorf("push 进行中时 c.mu 等待了 %v（上限 %v）：HandleIMEActivated 可能在 push 期间持有锁（回归）",
			lockWait, maxLockWait)
	}

	// 等待 HandleIMEActivated 完全结束再退出，避免 goroutine 泄漏污染其他测试
	select {
	case <-done:
	case <-time.After(pushDelay + 200*time.Millisecond):
		t.Error("HandleIMEActivated 未在预期时间内结束")
	}
}
