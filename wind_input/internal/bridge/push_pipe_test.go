//go:build windows

// Push pipe 回归测试——把已修复的 IPC bug 写成断言, 防止重构再次引入。
//
// 测试用真 winio.ListenPipe + DialPipe 的双端 pipe, 覆盖那些"内存 mock"
// 抓不到的内核行为 (sync I/O 串行化、broken pipe 信号、handle 引用计数)。
package bridge

import (
	"encoding/binary"
	"io"
	"log/slog"
	"net"
	"sync"
	"testing"
	"time"

	"github.com/Microsoft/go-winio"
	"github.com/huanfeng/wind_input/internal/ipc"
)

// newTestServer 构造一个只用 push pipe 路径的 Server, 跳过 MessageHandler
// (push pipe 不依赖 handler)。tests 不调 Start(), 只手动驱动 acceptPushClient。
func newTestServer(t *testing.T) *Server {
	t.Helper()
	s := NewServer(nil, slog.New(slog.NewTextHandler(io.Discard, nil)))
	return s
}

// startTestPushListener 起一个**独立 pipe 名**的 winio listener,
// 把每个 Accept 的 conn 交给 acceptPushClient。返回的 cleanup 关 listener。
func startTestPushListener(t *testing.T, s *Server) (pipeName string, cleanup func()) {
	t.Helper()
	pipeName = `\\.\pipe\windinput_bridge_test_` + uniqueSuffix(t)
	listener, err := winio.ListenPipe(pipeName, &winio.PipeConfig{
		MessageMode:      true,
		InputBufferSize:  16,
		OutputBufferSize: 65536,
	})
	if err != nil {
		t.Fatalf("listen test pipe: %v", err)
	}
	done := make(chan struct{})
	go func() {
		defer close(done)
		for {
			conn, err := listener.Accept()
			if err != nil {
				return
			}
			s.acceptPushClient(conn)
		}
	}()
	cleanup = func() {
		listener.Close()
		<-done
	}
	return pipeName, cleanup
}

func uniqueSuffix(t *testing.T) string {
	// 用测试名 + 当前时间纳秒做唯一后缀, 避免并发运行 / 重跑碰撞
	return t.Name() + "_" + time.Now().Format("150405.000000000")
}

func dialClient(t *testing.T, pipeName string) net.Conn {
	t.Helper()
	timeout := 2 * time.Second
	conn, err := winio.DialPipe(pipeName, &timeout)
	if err != nil {
		t.Fatalf("dial test pipe: %v", err)
	}
	return conn
}

func sendToken(t *testing.T, conn net.Conn, token uint64) {
	t.Helper()
	var buf [8]byte
	binary.LittleEndian.PutUint64(buf[:], token)
	if _, err := conn.Write(buf[:]); err != nil {
		t.Fatalf("send token: %v", err)
	}
}

func eventuallyTrue(t *testing.T, timeout time.Duration, msg string, cond func() bool) {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		if cond() {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("eventually: %s (timeout %v)", msg, timeout)
}

// 等待 server 端注册了至少 n 个 push client。
func waitForClientCount(t *testing.T, s *Server, want int, timeout time.Duration) {
	t.Helper()
	eventuallyTrue(t, timeout, "expected push client count", func() bool {
		s.pushMu.RLock()
		got := len(s.pushClients)
		s.pushMu.RUnlock()
		return got >= want
	})
}

// ──────────────────────────────────────────────────────────────────────────────
// 单元测试：pushClient 行为
// ──────────────────────────────────────────────────────────────────────────────

// TestPushClient_EnqueueBroadcastDropsWhenFull: 队列满后 enqueue 返回 false (drop)。
func TestPushClient_EnqueueBroadcastDropsWhenFull(t *testing.T) {
	// 直接构造一个 outbound 容量已知的 pushClient (绕开 newPushClient 的 net.Conn 依赖)
	c := &pushClient{outbound: make(chan []byte, pushOutboundBufferSize)}
	for i := 0; i < pushOutboundBufferSize; i++ {
		if !c.enqueueBroadcast([]byte{byte(i)}) {
			t.Fatalf("enqueue %d should succeed (queue not full yet)", i)
		}
	}
	if c.enqueueBroadcast([]byte{0xFF}) {
		t.Fatal("regression: enqueue should return false when queue is full")
	}
	// drain 一条, 下次 enqueue 应该再次成功
	<-c.outbound
	if !c.enqueueBroadcast([]byte{0xEE}) {
		t.Fatal("enqueue should recover after drain")
	}
}

// TestPushClient_ShutdownIdempotent: 多次 shutdown 不 panic。
func TestPushClient_ShutdownIdempotent(t *testing.T) {
	defer func() {
		if r := recover(); r != nil {
			t.Fatalf("regression: shutdown panicked on repeated call: %v", r)
		}
	}()
	// 用 net.Pipe 做一个无 Fd() 的 fake conn——shutdown 走 Disconnect 路径会找不到
	// PipeConn 接口, 直接 Close()。Close() 也是幂等的。
	a, b := net.Pipe()
	defer a.Close()
	c := &pushClient{conn: b, outbound: make(chan []byte, 4)}
	c.shutdown()
	c.shutdown() // 第二次应该 no-op
	c.shutdown() // 第三次也应该 no-op
}

// ──────────────────────────────────────────────────────────────────────────────
// 端到端回归测试：真 winio pipe
// ──────────────────────────────────────────────────────────────────────────────

// TestPushPipe_E2E_TokenHandshake: client 连接 + token 注册的基本流程。
func TestPushPipe_E2E_TokenHandshake(t *testing.T) {
	s := newTestServer(t)
	pipeName, cleanup := startTestPushListener(t, s)
	defer cleanup()

	client := dialClient(t, pipeName)
	defer client.Close()

	// 服务端在连接时同步推 CMD_SERVICE_READY (10 字节 header)
	var hdr [ipc.HeaderSize]byte
	if _, err := io.ReadFull(client, hdr[:]); err != nil {
		t.Fatalf("read SERVICE_READY: %v", err)
	}
	gotCmd := binary.LittleEndian.Uint16(hdr[2:4])
	if gotCmd != ipc.CmdServiceReady {
		t.Fatalf("first message cmd=0x%04X, want CmdServiceReady=0x%04X", gotCmd, ipc.CmdServiceReady)
	}

	// 发送 token, 等待 server 端注册
	const testToken uint64 = 0xC0FFEE
	sendToken(t, client, testToken)
	eventuallyTrue(t, 1*time.Second, "token registered", func() bool {
		s.pushMu.RLock()
		_, ok := s.tokenToPushHandle[testToken]
		s.pushMu.RUnlock()
		return ok
	})

	// 等服务端注册 client 进 pushClients map
	waitForClientCount(t, s, 1, 1*time.Second)
}

// TestPushPipe_E2E_ReaderDoesNotBlockWriter: **关键回归测试**.
// 历史 bug: phase-2 reader 在同 handle 上 sync ReadFile park,
// writer 的 sync WriteFile 被 Windows 内核串行化, 永远不返回.
// 现在 go-winio overlapped I/O 不应有这个问题——本测试验证它。
//
// 测试方法: 模拟 reader park (客户端连上但不发任何额外数据), 同时服务端
// 触发 push, 验证 push 在合理时间内完成 (远低于"sync 串行化"的几十秒卡死)。
func TestPushPipe_E2E_ReaderDoesNotBlockWriter(t *testing.T) {
	s := newTestServer(t)
	pipeName, cleanup := startTestPushListener(t, s)
	defer cleanup()

	client := dialClient(t, pipeName)
	defer client.Close()

	// 让 client 在 background 读取所有消息 (模拟 C++ async reader)
	clientReceived := make(chan uint16, 32)
	go func() {
		for {
			var hdr [ipc.HeaderSize]byte
			if _, err := io.ReadFull(client, hdr[:]); err != nil {
				return
			}
			cmd := binary.LittleEndian.Uint16(hdr[2:4])
			payloadLen := binary.LittleEndian.Uint32(hdr[4:8])
			if payloadLen > 0 {
				_, _ = io.CopyN(io.Discard, client, int64(payloadLen))
			}
			select {
			case clientReceived <- cmd:
			default:
			}
		}
	}()

	// 跳过 SERVICE_READY
	<-clientReceived

	// 发送 token, 走完握手
	sendToken(t, client, 0xABCD)
	waitForClientCount(t, s, 1, 1*time.Second)

	// 拿到 server 端 pushClient 引用
	s.pushMu.RLock()
	var pc *pushClient
	for _, c := range s.pushClients {
		pc = c
	}
	s.pushMu.RUnlock()
	if pc == nil {
		t.Fatal("no pushClient registered")
	}

	// 触发 push: 把 state push 入队, writer goroutine 应当立即消费并写出。
	// 关键断言: writer 不应被 phase-2 reader 串行化阻塞。
	statePush := s.codec.EncodeStatePush(true, false, true, false, false, "中")
	if !pc.enqueueBroadcast(statePush) {
		t.Fatal("enqueue should succeed")
	}

	// 期望 client 在 < 200ms 内收到 (winio overlapped 应当 μs 级)。
	// 如果回归到 sync 串行化, 这里会 timeout (旧版会卡 30s+ 直到 reader 返回)。
	select {
	case cmd := <-clientReceived:
		if cmd != ipc.CmdStatePush {
			t.Fatalf("expected CmdStatePush=0x%04X, got 0x%04X", ipc.CmdStatePush, cmd)
		}
	case <-time.After(200 * time.Millisecond):
		t.Fatal("regression: writer was blocked by reader (sync I/O serialization); " +
			"check that pipe is FILE_FLAG_OVERLAPPED via winio")
	}
}

// TestPushPipe_E2E_DeadClientCleanedUp: client 主动 Close 后, server 端
// 应当通过 phase-2 reader 的 io.EOF 检测到, 在合理时间内从 pushClients 移除。
//
// 历史 bug: 当时用 sync ReadFile park, 我们删过 phase-2 reader,
// 改成"靠下次写失败再清理"。winio overlapped 安全地恢复了 phase-2 监听。
func TestPushPipe_E2E_DeadClientCleanedUp(t *testing.T) {
	s := newTestServer(t)
	pipeName, cleanup := startTestPushListener(t, s)
	defer cleanup()

	client := dialClient(t, pipeName)

	// 跳过 SERVICE_READY
	var hdr [ipc.HeaderSize]byte
	_, _ = io.ReadFull(client, hdr[:])

	sendToken(t, client, 0xDEAD)
	waitForClientCount(t, s, 1, 1*time.Second)

	// Client 主动关闭模拟 TSF 进程退出
	if err := client.Close(); err != nil {
		t.Fatalf("client close: %v", err)
	}

	// Server 端 phase-2 reader 应当在 1s 内通过 io.EOF 触发 cleanup
	eventuallyTrue(t, 1*time.Second, "dead client cleaned up", func() bool {
		s.pushMu.RLock()
		n := len(s.pushClients)
		s.pushMu.RUnlock()
		return n == 0
	})
}

// TestPushPipe_E2E_StaleTokenReplaced: 同 token 重连场景。
// 旧 handle 必失效, 应该主动清理, 否则 explorer.exe 多实例下会积累幽灵 client。
func TestPushPipe_E2E_StaleTokenReplaced(t *testing.T) {
	s := newTestServer(t)
	pipeName, cleanup := startTestPushListener(t, s)
	defer cleanup()

	const sharedToken uint64 = 0xBABE

	// 第一个 client 连上并注册 token
	c1 := dialClient(t, pipeName)
	defer c1.Close()
	var hdr [ipc.HeaderSize]byte
	_, _ = io.ReadFull(c1, hdr[:])
	sendToken(t, c1, sharedToken)
	waitForClientCount(t, s, 1, 1*time.Second)

	// 第二个 client 用**相同** token 连接, 应该顶替掉第一个
	c2 := dialClient(t, pipeName)
	defer c2.Close()
	_, _ = io.ReadFull(c2, hdr[:])
	sendToken(t, c2, sharedToken)

	// 在合理时间内, pushClients 应当只有 1 个 (c2), 第一个被清掉
	eventuallyTrue(t, 1*time.Second, "stale handle replaced", func() bool {
		s.pushMu.RLock()
		defer s.pushMu.RUnlock()
		// token 映射的应该是 c2 的 handle
		if len(s.pushClients) > 1 {
			return false
		}
		_, ok := s.tokenToPushHandle[sharedToken]
		return ok && len(s.pushClients) == 1
	})
}

// TestPushPipe_E2E_QueueFullDropsLatest: client 读得慢时, 入队第 17 条开始 drop。
//
// 测试用法: client 连上但不读, 服务端 outbound 缓冲容量是 16,
// 第 17 次 enqueueBroadcast 应该返回 false (drop)。
func TestPushPipe_E2E_QueueFullDropsLatest(t *testing.T) {
	s := newTestServer(t)
	pipeName, cleanup := startTestPushListener(t, s)
	defer cleanup()

	client := dialClient(t, pipeName)
	defer client.Close()

	// 不启动 client reader (模拟 slow/blocked C++ async reader)。
	// SERVICE_READY 会卡在 kernel buffer 但不影响后续 enqueue。

	sendToken(t, client, 0x1234)
	waitForClientCount(t, s, 1, 1*time.Second)

	s.pushMu.RLock()
	var pc *pushClient
	for _, c := range s.pushClients {
		pc = c
	}
	s.pushMu.RUnlock()
	if pc == nil {
		t.Fatal("no pushClient")
	}

	// 1) 把 outbound 队列灌满。msg 体积小让 writer goroutine 不至于太快
	// drain 出来——但如果 writer 真的 drain 干净, 我们的"满"判断就不成立。
	// 用一个大 msg 让 writer 在 client 不读时卡在 conn.Write 内核 buffer 上,
	// 这样后续入队都堆在 channel 里。
	bigMsg := make([]byte, 65000) // 接近 64KB pipe buffer, 一次写到底
	if !pc.enqueueBroadcast(bigMsg) {
		t.Fatal("first enqueue should succeed")
	}
	// 等 writer 进入 WriteFile 卡住
	time.Sleep(50 * time.Millisecond)

	// 2) 后续小 msg 填满 outbound (容量 pushOutboundBufferSize=16)
	successCount := 0
	dropCount := 0
	for i := 0; i < pushOutboundBufferSize+10; i++ {
		if pc.enqueueBroadcast([]byte{byte(i)}) {
			successCount++
		} else {
			dropCount++
		}
	}
	// 至少应该有 drop 出现
	if dropCount == 0 {
		t.Fatalf("regression: no drops despite slow client; successCount=%d, dropCount=%d", successCount, dropCount)
	}
	t.Logf("queue full behavior verified: %d succeeded, %d dropped", successCount, dropCount)
}

// TestPushPipe_E2E_ConcurrentWritesSerialized: 多 goroutine 并发 enqueue
// 的消息不应混淆 (writer goroutine + 同步直写共享 conn 时由 pushClient.mu 串行化)。
func TestPushPipe_E2E_ConcurrentWritesSerialized(t *testing.T) {
	s := newTestServer(t)
	pipeName, cleanup := startTestPushListener(t, s)
	defer cleanup()

	client := dialClient(t, pipeName)
	defer client.Close()

	// SERVICE_READY
	var hdr [ipc.HeaderSize]byte
	_, _ = io.ReadFull(client, hdr[:])

	sendToken(t, client, 0xCAFE)
	waitForClientCount(t, s, 1, 1*time.Second)

	s.pushMu.RLock()
	var pc *pushClient
	for _, c := range s.pushClients {
		pc = c
	}
	s.pushMu.RUnlock()
	if pc == nil {
		t.Fatal("no pushClient")
	}

	// Client 端持续读取所有消息
	received := make(chan int, 200)
	go func() {
		for {
			var h [ipc.HeaderSize]byte
			if _, err := io.ReadFull(client, h[:]); err != nil {
				return
			}
			plen := binary.LittleEndian.Uint32(h[4:8])
			payload := make([]byte, plen)
			if plen > 0 {
				if _, err := io.ReadFull(client, payload); err != nil {
					return
				}
			}
			received <- int(plen)
		}
	}()

	// 启 5 个 goroutine 并发触发 push (走 enqueueBroadcast → writer)
	var wg sync.WaitGroup
	const perGoroutine = 20
	for g := 0; g < 5; g++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for i := 0; i < perGoroutine; i++ {
				msg := s.codec.EncodeStatePush(i%2 == 0, false, true, true, false, "中")
				_ = pc.enqueueBroadcast(msg) // 满了允许丢, 这里只测不混淆
			}
		}()
	}
	wg.Wait()

	// 给 client 时间读完
	deadline := time.Now().Add(2 * time.Second)
	count := 0
	for time.Now().Before(deadline) {
		select {
		case <-received:
			count++
		case <-time.After(100 * time.Millisecond):
			if count > 0 {
				goto done // 没新消息进来, 认为 drain 完毕
			}
		}
	}
done:
	if count < pushOutboundBufferSize {
		t.Fatalf("expected at least %d messages received, got %d", pushOutboundBufferSize, count)
	}
	t.Logf("concurrent writes delivered %d messages (some may have been dropped under queue pressure)", count)
}
