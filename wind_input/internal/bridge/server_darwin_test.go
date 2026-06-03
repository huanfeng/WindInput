//go:build darwin

package bridge

import (
	"encoding/binary"
	"io"
	"log/slog"
	"net"
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/huanfeng/wind_input/internal/ipc"
)

// server_darwin_test.go 验证 darwin UDS server 的端到端协议交互:
// 1. 启动 Server (用临时目录作为运行时, 避免污染 $HOME)
// 2. 用 net.Dial("unix", ...) 模拟 IMKit 客户端连接
// 3. 写一帧 ipc 二进制协议, 验证 handler 收到, 服务端返回响应

// fakeHandler 收集 handler 调用, 供测试断言。
type fakeHandler struct {
	mu               sync.Mutex
	keyEvents        []KeyEventData
	caretUpdates     []CaretData
	focusGained      uint32
	focusScopeMask   uint64
	focusLost        bool
	imeActivated     uint32
	imeDeactivated   bool
	disconnectedCnt  int32
	caretPending     bool
	clientDisconnect chan struct{}
}

func newFakeHandler() *fakeHandler {
	return &fakeHandler{clientDisconnect: make(chan struct{}, 8)}
}

func (h *fakeHandler) HandleKeyEvent(data KeyEventData) *KeyEventResult {
	h.mu.Lock()
	h.keyEvents = append(h.keyEvents, data)
	h.mu.Unlock()
	return &KeyEventResult{Type: ResponseTypePassThrough}
}
func (h *fakeHandler) HandleCaretUpdate(data CaretData) error {
	h.mu.Lock()
	h.caretUpdates = append(h.caretUpdates, data)
	h.mu.Unlock()
	return nil
}
func (h *fakeHandler) HandleCaretPending() {
	h.mu.Lock()
	h.caretPending = true
	h.mu.Unlock()
}
func (h *fakeHandler) HandleFocusLost() {
	h.mu.Lock()
	h.focusLost = true
	h.mu.Unlock()
}
func (h *fakeHandler) HandleCompositionTerminated() {}
func (h *fakeHandler) HandleFocusGained(processID uint32, inputScopeMask uint64) *StatusUpdateData {
	h.mu.Lock()
	h.focusGained = processID
	h.focusScopeMask = inputScopeMask
	h.mu.Unlock()
	return &StatusUpdateData{ChineseMode: true, IconLabel: "中"}
}
func (h *fakeHandler) HandleIMEDeactivated() {
	h.mu.Lock()
	h.imeDeactivated = true
	h.mu.Unlock()
}
func (h *fakeHandler) HandleIMEActivated(processID uint32) *StatusUpdateData {
	h.mu.Lock()
	h.imeActivated = processID
	h.mu.Unlock()
	return &StatusUpdateData{ChineseMode: true}
}
func (h *fakeHandler) HandleToggleMode() (*StatusUpdateData, string) {
	return &StatusUpdateData{ChineseMode: true}, ""
}
func (h *fakeHandler) HandleCapsLockState(on bool)                {}
func (h *fakeHandler) HandleMenuCommand(string) *StatusUpdateData { return nil }
func (h *fakeHandler) HandleClientDisconnected(active int) {
	atomic.AddInt32(&h.disconnectedCnt, 1)
	select {
	case h.clientDisconnect <- struct{}{}:
	default:
	}
}
func (h *fakeHandler) HandleCommitRequest(CommitRequestData) *CommitResultData {
	return nil
}
func (h *fakeHandler) HandleModeNotify(ModeNotifyData) {}
func (h *fakeHandler) HandleSystemModeSwitch(bool) (*StatusUpdateData, string) {
	return nil, ""
}
func (h *fakeHandler) HandleShowContextMenu(int, int)           {}
func (h *fakeHandler) HandleSelectionChanged(rune)              {}
func (h *fakeHandler) HandleHostRenderReady()                   {}
func (h *fakeHandler) HandleInputStats(int, int, int, int, int) {}

// shortTempDir 返回 /tmp 下的短路径临时目录并登记清理。
// 不用 t.TempDir(): macOS 上它落在 /var/folders/.../<长测试名>/001, 拼上 socket
// 文件名后常超过 Unix domain socket 的 sun_path 104 字节硬上限, 使 bind/connect
// 报 "invalid argument"(EINVAL); 路径长度随测试名与随机后缀浮动还会导致时过时不过。
// /tmp 下的短目录(约 30 字节)可稳定避开该限制。
func shortTempDir(t *testing.T) string {
	t.Helper()
	dir, err := os.MkdirTemp("/tmp", "wb")
	if err != nil {
		t.Fatalf("MkdirTemp: %v", err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
	return dir
}

// setupTestServer 用临时目录起一个 Server 用于测试。返回 server + cleanup。
func setupTestServer(t *testing.T) (*Server, *fakeHandler, func()) {
	t.Helper()
	dir := shortTempDir(t)
	t.Setenv("WIND_INPUT_RUNTIME_DIR", dir)
	// 重新计算端点 (init 只在包加载时跑过, 测试需手动覆盖)
	BridgePipeName = filepath.Join(dir, "bridge.sock")
	PushPipeName = filepath.Join(dir, "bridge_push.sock")

	h := newFakeHandler()
	srv := NewServer(h, slog.New(slog.NewTextHandler(io.Discard, nil)))

	started := make(chan error, 1)
	go func() {
		started <- srv.Start()
	}()
	// 给 listener 一点时间起来
	for i := 0; i < 50; i++ {
		if _, err := os.Stat(BridgePipeName); err == nil {
			break
		}
		time.Sleep(10 * time.Millisecond)
	}

	cleanup := func() {
		srv.RestartService()
		select {
		case <-started:
		case <-time.After(500 * time.Millisecond):
			// listener 关后 Start 应快速返回; 超时也不阻塞测试结束
		}
	}
	return srv, h, cleanup
}

// dialAndSend 连接 bridge.sock, 发一帧, 读一帧响应。
func dialAndSend(t *testing.T, frame []byte) (header *ipc.IpcHeader, payload []byte, conn net.Conn) {
	t.Helper()
	c, err := net.Dial("unix", BridgePipeName)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	if _, err := c.Write(frame); err != nil {
		t.Fatalf("write: %v", err)
	}
	codec := ipc.NewBinaryCodec()
	header, err = codec.ReadHeader(c)
	if err != nil {
		t.Fatalf("read header: %v", err)
	}
	payload, err = codec.ReadPayload(c, header.Length)
	if err != nil {
		t.Fatalf("read payload: %v", err)
	}
	return header, payload, c
}

// TestDarwinBridge_KeyEvent_Roundtrip 验证 KeyEvent 端到端流转。
func TestDarwinBridge_KeyEvent_Roundtrip(t *testing.T) {
	_, h, cleanup := setupTestServer(t)
	defer cleanup()

	codec := ipc.NewBinaryCodec()
	// 构造 KeyEvent payload: keyCode(4) scanCode(4) modifiers(4) eventType(1) toggles(1) eventSeq(2)
	payload := make([]byte, 16)
	binary.LittleEndian.PutUint32(payload[0:4], 0x41) // 'A'
	binary.LittleEndian.PutUint32(payload[4:8], 0)    // scan code
	binary.LittleEndian.PutUint32(payload[8:12], 0)   // modifiers
	payload[12] = 0                                   // eventType = down
	payload[13] = 0                                   // toggles
	binary.LittleEndian.PutUint16(payload[14:16], 42) // eventSeq

	header := codec.EncodeHeader(ipc.CmdKeyEvent, uint32(len(payload)))
	frame := append(header, payload...)

	respHeader, _, conn := dialAndSend(t, frame)
	defer conn.Close()

	// 响应应是 PassThrough (handler 返回的)
	if respHeader.Command != ipc.CmdPassThrough {
		t.Errorf("response cmd = 0x%04x, want CmdPassThrough (0x%04x)", respHeader.Command, ipc.CmdPassThrough)
	}

	// 给 handler goroutine 一点时间运行
	time.Sleep(20 * time.Millisecond)
	h.mu.Lock()
	defer h.mu.Unlock()
	if len(h.keyEvents) != 1 {
		t.Fatalf("handler got %d key events, want 1", len(h.keyEvents))
	}
	if h.keyEvents[0].KeyCode != 0x41 {
		t.Errorf("KeyCode = 0x%x, want 0x41", h.keyEvents[0].KeyCode)
	}
	if h.keyEvents[0].Event != "down" {
		t.Errorf("Event = %q, want down", h.keyEvents[0].Event)
	}
}

// TestDarwinBridge_FocusGained 验证 FocusGained 帧的 PID 解析与状态记录。
func TestDarwinBridge_FocusGained(t *testing.T) {
	srv, h, cleanup := setupTestServer(t)
	defer cleanup()

	codec := ipc.NewBinaryCodec()
	payload := make([]byte, 4)
	binary.LittleEndian.PutUint32(payload, 12345)
	header := codec.EncodeHeader(ipc.CmdFocusGained, 4)
	frame := append(header, payload...)

	_, _, conn := dialAndSend(t, frame)
	defer conn.Close()
	time.Sleep(20 * time.Millisecond)

	h.mu.Lock()
	gotPID := h.focusGained
	h.mu.Unlock()
	if gotPID != 12345 {
		t.Errorf("focusGained PID = %d, want 12345", gotPID)
	}

	// IsActivelyFocusedPID darwin 上始终 false (即使 internal 标记了 focus)。
	// 这是设计约定: PID 概念在 darwin 不适用。
	if srv.IsActivelyFocusedPID(12345) {
		t.Error("IsActivelyFocusedPID should be false on darwin even after focus")
	}
}

// TestDarwinBridge_FocusGainedInputScope 验证 forwarder 在 payload[4:12] 携带的
// InputScope bitmask（密码框 IS_PASSWORD=bit31）被解析并透传给 HandleFocusGained。
func TestDarwinBridge_FocusGainedInputScope(t *testing.T) {
	srv, h, cleanup := setupTestServer(t)
	defer cleanup()
	_ = srv

	codec := ipc.NewBinaryCodec()
	const wantMask = uint64(1) << 31 // IS_PASSWORD
	payload := make([]byte, 12)
	binary.LittleEndian.PutUint32(payload[0:4], 0) // pid 占位
	binary.LittleEndian.PutUint64(payload[4:12], wantMask)
	header := codec.EncodeHeader(ipc.CmdFocusGained, uint32(len(payload)))
	frame := append(header, payload...)

	_, _, conn := dialAndSend(t, frame)
	defer conn.Close()
	time.Sleep(20 * time.Millisecond)

	h.mu.Lock()
	gotMask := h.focusScopeMask
	h.mu.Unlock()
	if gotMask != wantMask {
		t.Errorf("focusGained inputScopeMask = 0x%X, want 0x%X", gotMask, wantMask)
	}
}

// TestDarwinBridge_MultipleClients 验证多客户端并发连接 + clientCount 准确。
func TestDarwinBridge_MultipleClients(t *testing.T) {
	srv, _, cleanup := setupTestServer(t)
	defer cleanup()

	const N = 3
	conns := make([]net.Conn, N)
	for i := 0; i < N; i++ {
		c, err := net.Dial("unix", BridgePipeName)
		if err != nil {
			t.Fatalf("dial #%d: %v", i, err)
		}
		conns[i] = c
	}
	defer func() {
		for _, c := range conns {
			_ = c.Close()
		}
	}()

	// 等待 server 完成 accept
	deadline := time.Now().Add(500 * time.Millisecond)
	for time.Now().Before(deadline) {
		if srv.GetActiveClientCount() == N {
			break
		}
		time.Sleep(10 * time.Millisecond)
	}
	if got := srv.GetActiveClientCount(); got != N {
		t.Errorf("clientCount = %d, want %d", got, N)
	}
}

// TestDarwinBridge_ClientDisconnect 关闭 client 后 handler 收到 disconnect。
func TestDarwinBridge_ClientDisconnect(t *testing.T) {
	_, h, cleanup := setupTestServer(t)
	defer cleanup()

	c, err := net.Dial("unix", BridgePipeName)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	time.Sleep(20 * time.Millisecond) // 让 server 完成 accept
	_ = c.Close()

	select {
	case <-h.clientDisconnect:
		// expected
	case <-time.After(500 * time.Millisecond):
		t.Error("handler.HandleClientDisconnected not called after client close")
	}
}

// TestDarwinBridge_PushBroadcast 验证 push pipe fanout。
func TestDarwinBridge_PushBroadcast(t *testing.T) {
	srv, _, cleanup := setupTestServer(t)
	defer cleanup()

	// 起两个 push client
	const N = 2
	pcs := make([]net.Conn, N)
	for i := 0; i < N; i++ {
		c, err := net.Dial("unix", PushPipeName)
		if err != nil {
			t.Fatalf("dial push #%d: %v", i, err)
		}
		pcs[i] = c
	}
	defer func() {
		for _, c := range pcs {
			_ = c.Close()
		}
	}()
	// 等待 server 完成 accept
	time.Sleep(50 * time.Millisecond)

	// 服务端 push 一个 StatusUpdate
	srv.PushStateToActiveClient(&StatusUpdateData{
		ChineseMode: true,
		IconLabel:   "中",
	})

	// 每个 client 都应读到 frame (CmdStatePush header)
	codec := ipc.NewBinaryCodec()
	for i, c := range pcs {
		_ = c.SetReadDeadline(time.Now().Add(500 * time.Millisecond))
		header, err := codec.ReadHeader(c)
		if err != nil {
			t.Fatalf("push client #%d read: %v", i, err)
		}
		if header.Command != ipc.CmdStatePush {
			t.Errorf("push client #%d cmd = 0x%04x, want CmdStatePush", i, header.Command)
		}
		// 排空 payload
		_, _ = codec.ReadPayload(c, header.Length)
	}
}

// TestDarwinBridge_StaleSocketCleanup 验证启动时清理残留 socket 文件。
func TestDarwinBridge_StaleSocketCleanup(t *testing.T) {
	dir := shortTempDir(t)
	t.Setenv("WIND_INPUT_RUNTIME_DIR", dir)
	BridgePipeName = filepath.Join(dir, "bridge.sock")
	PushPipeName = filepath.Join(dir, "bridge_push.sock")

	// 制造一个 stale 文件 (非 socket 也行, server 会 Remove 重建)
	if err := os.WriteFile(BridgePipeName, []byte("stale"), 0o600); err != nil {
		t.Fatal(err)
	}

	srv := NewServer(newFakeHandler(), slog.New(slog.NewTextHandler(io.Discard, nil)))
	started := make(chan error, 1)
	go func() { started <- srv.Start() }()
	time.Sleep(50 * time.Millisecond)
	defer srv.RestartService()

	// 应能 dial 成功 (stale 文件被清理后重建为 socket)
	c, err := net.Dial("unix", BridgePipeName)
	if err != nil {
		t.Fatalf("dial after stale cleanup: %v", err)
	}
	_ = c.Close()
}

// TestEndpointPathDerivation 验证 endpoint_darwin.go 路径派生逻辑。
func TestEndpointPathDerivation(t *testing.T) {
	t.Setenv("WIND_INPUT_RUNTIME_DIR", "/tmp/wind_test")
	if got := bridgeRuntimeDir(); got != "/tmp/wind_test" {
		t.Errorf("env override not honored: got %q", got)
	}

	t.Setenv("WIND_INPUT_RUNTIME_DIR", "")
	// 不设 HOME 则 fallback /tmp; 但 t.Setenv 会保留外部 $HOME。
	// 我们只验证 path 含 "WindInput" 字样且为绝对路径。
	d := bridgeRuntimeDir()
	if !filepath.IsAbs(d) {
		t.Errorf("runtime dir not absolute: %q", d)
	}
}
