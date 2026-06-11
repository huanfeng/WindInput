//go:build windows

// IMEActivated / FocusGained 异步化的契约回归测试。
//
// 关键契约 (一旦回归会让工具栏不显示 / LangBar 状态不同步):
//  1. processRequest 收到 CmdIMEActivated / CmdFocusGained 时**不得**调用
//     HandleIMEActivated / HandleFocusGained, 必须立即返回 EncodeAck()。
//     handler 调用搬到 handleClient 在 Ack 写出后才进行。
//  2. runActivationHandlerAndPush 必须按 header.Command 调对应 handler,
//     并将返回的 StatusUpdateData 以 CmdActivationStatusPush 推到 active client。
//  3. handler 返回 nil 状态时, 不得 push 空状态包 (避免覆盖 C++ 端 mirror)。
package bridge

import (
	"encoding/binary"
	"io"
	"log/slog"
	"sync/atomic"
	"testing"
	"time"

	"github.com/huanfeng/wind_input/internal/ipc"
	"golang.org/x/sys/windows"
)

// fakeMessageHandler 是 MessageHandler 的最小实现:
// 关心的方法记录调用次数与入参, 其它方法返回零值。
type fakeMessageHandler struct {
	imeActivatedCalls  atomic.Int32
	focusGainedCalls   atomic.Int32
	lastIMEActivatedID atomic.Uint32
	lastFocusGainedID  atomic.Uint32

	// 让测试者注入返回值。
	imeActivatedReturn *StatusUpdateData
	focusGainedReturn  *StatusUpdateData
}

func (f *fakeMessageHandler) HandleKeyEvent(KeyEventData) *KeyEventResult { return nil }
func (f *fakeMessageHandler) HandleCaretUpdate(CaretData) error           { return nil }
func (f *fakeMessageHandler) HandleCaretPending()                         {}
func (f *fakeMessageHandler) HandleFocusLost()                            {}
func (f *fakeMessageHandler) HandleCompositionTerminated()                {}
func (f *fakeMessageHandler) HandleFocusGained(processID uint32, inputScopeMask uint64) *StatusUpdateData {
	f.focusGainedCalls.Add(1)
	f.lastFocusGainedID.Store(processID)
	return f.focusGainedReturn
}
func (f *fakeMessageHandler) HandleIMEDeactivated() {}
func (f *fakeMessageHandler) HandleIMEActivated(processID uint32) *StatusUpdateData {
	f.imeActivatedCalls.Add(1)
	f.lastIMEActivatedID.Store(processID)
	return f.imeActivatedReturn
}
func (f *fakeMessageHandler) HandleToggleMode() (*StatusUpdateData, string) { return nil, "" }
func (f *fakeMessageHandler) HandleCapsLockState(bool)                      {}
func (f *fakeMessageHandler) HandleMenuCommand(string) *StatusUpdateData    { return nil }
func (f *fakeMessageHandler) HandleClientDisconnected(int)                  {}
func (f *fakeMessageHandler) HandlePushClientConnected(uint32)              {}
func (f *fakeMessageHandler) HandleCommitRequest(CommitRequestData) *CommitResultData {
	return nil
}
func (f *fakeMessageHandler) HandleModeNotify(ModeNotifyData) {}
func (f *fakeMessageHandler) HandleSystemModeSwitch(bool) (*StatusUpdateData, string) {
	return nil, ""
}
func (f *fakeMessageHandler) HandleShowContextMenu(int, int)           {}
func (f *fakeMessageHandler) HandleSelectionChanged(rune)              {}
func (f *fakeMessageHandler) HandleHostRenderReady()                   {}
func (f *fakeMessageHandler) HandleInputStats(int, int, int, int, int) {}
func (f *fakeMessageHandler) GetCurrentMode() (bool, bool)             { return true, false }

func newServerWithFakeHandler(t *testing.T, h MessageHandler) *Server {
	t.Helper()
	return NewServer(h, slog.New(slog.NewTextHandler(io.Discard, nil)))
}

// installFakePushClient 在 server 的 pushClients map 里注入一个能被读取的 client:
// 出端是 outbound channel, 测试代码直接从 channel 拿到 PushActivationStatusToActiveClient
// 入队的字节, 验证编码正确。
//
// 同时注册 PID/token 路由, 让 pushToActiveClient 能找到这条 client。
func installFakePushClient(s *Server, pid uint32, token uint64) (*pushClient, windows.Handle) {
	// handle 用一个稳定的占位值。pushClient 字段中 conn 没用 (我们不真的写网络),
	// 但 outbound channel 必须就位让 enqueueBroadcast 能落地。
	handle := windows.Handle(0xDEAD0000 + uintptr(pid))
	pc := &pushClient{
		handle:   handle,
		outbound: make(chan []byte, pushOutboundBufferSize),
	}
	s.pushMu.Lock()
	s.pushClients[handle] = pc
	s.pushClientsByPID[pid] = handle
	s.pushHandleToPID[handle] = pid
	if token != 0 {
		s.tokenToPushHandle[token] = handle
		s.pushHandleToToken[handle] = token
	}
	s.pushMu.Unlock()

	s.activeMu.Lock()
	s.activeProcessID = pid
	s.activeToken = token
	s.activeMu.Unlock()
	return pc, handle
}

// TestProcessRequest_IMEActivatedReturnsAckWithoutInvokingHandler 守住契约 1
// 针对 CmdIMEActivated:
//   - 返回值字节 == EncodeAck()
//   - handler.HandleIMEActivated **未被** processRequest 调用
//     (调用搬到 handleClient 在 Ack 写出后才发生)
func TestProcessRequest_IMEActivatedReturnsAckWithoutInvokingHandler(t *testing.T) {
	h := &fakeMessageHandler{}
	s := newServerWithFakeHandler(t, h)

	// payload: IMEActivatedPayload = 8 字节 token (与 wind_tsf 端的结构一致)。
	var payload [8]byte
	binary.LittleEndian.PutUint64(payload[:], 0xCAFEBABE)

	header := &ipc.IpcHeader{Version: ipc.ProtocolVersion, Command: ipc.CmdIMEActivated, Length: uint32(len(payload))}
	resp := s.processRequest(header, payload[:], 1, 12345)

	wantAck := s.codec.EncodeAck()
	if string(resp) != string(wantAck) {
		t.Fatalf("processRequest for CmdIMEActivated should return Ack bytes; got len=%d want len=%d", len(resp), len(wantAck))
	}
	if got := h.imeActivatedCalls.Load(); got != 0 {
		t.Fatalf("processRequest 不应该直接调用 HandleIMEActivated (这是 handleClient 的责任); got=%d", got)
	}

	// activeProcessID 必须在 processRequest 顶部已更新 (push 路由依赖它就绪)。
	s.activeMu.RLock()
	gotPID := s.activeProcessID
	s.activeMu.RUnlock()
	if gotPID != 12345 {
		t.Fatalf("activeProcessID 未在 processRequest 同步路径里更新; got=%d want=12345", gotPID)
	}
}

// TestProcessRequest_FocusGainedReturnsAckWithoutInvokingHandler 守住契约 1
// 针对 CmdFocusGained。FocusGained 同步路径允许 HandleCaretUpdate (纯字段写入),
// 但**绝不**调用 HandleFocusGained。
func TestProcessRequest_FocusGainedReturnsAckWithoutInvokingHandler(t *testing.T) {
	h := &fakeMessageHandler{}
	s := newServerWithFakeHandler(t, h)

	// payload: FocusGainedPayload = CaretPayload(20B) + clientToken(8B) = 28 字节。
	var payload [28]byte
	// caret.x = 100, caret.y = 200, caret.height = 16
	binary.LittleEndian.PutUint32(payload[0:4], 100)
	binary.LittleEndian.PutUint32(payload[4:8], 200)
	binary.LittleEndian.PutUint32(payload[8:12], 16)
	// compositionStart 留 0
	binary.LittleEndian.PutUint64(payload[20:28], 0xCAFEBABE)

	header := &ipc.IpcHeader{Version: ipc.ProtocolVersion, Command: ipc.CmdFocusGained, Length: uint32(len(payload))}
	resp := s.processRequest(header, payload[:], 2, 54321)

	if string(resp) != string(s.codec.EncodeAck()) {
		t.Fatalf("processRequest for CmdFocusGained should return Ack bytes")
	}
	if got := h.focusGainedCalls.Load(); got != 0 {
		t.Fatalf("processRequest 不应该直接调用 HandleFocusGained; got=%d", got)
	}
}

// TestRunActivationHandlerAndPush_IMEActivatedEncodesAndPushes 守住契约 2:
// - 按 header.Command 调用对应 handler;
// - 将 handler 返回的状态以 CmdActivationStatusPush 推到 active client 的 outbound;
// - 编码字段 (flags / iconLabel) 与 status 一致。
func TestRunActivationHandlerAndPush_IMEActivatedEncodesAndPushes(t *testing.T) {
	h := &fakeMessageHandler{
		imeActivatedReturn: &StatusUpdateData{
			ChineseMode:        true,
			FullWidth:          false,
			ChinesePunctuation: true,
			ToolbarVisible:     true,
			CapsLock:           false,
			IconLabel:          "中",
			KeyDownHotkeys:     []uint32{0x12345678},
		},
	}
	s := newServerWithFakeHandler(t, h)

	const pid uint32 = 0xABCD
	const token uint64 = 0xBEEF
	pc, _ := installFakePushClient(s, pid, token)

	header := &ipc.IpcHeader{Command: ipc.CmdIMEActivated}
	s.runActivationHandlerAndPush(header, nil, 1, pid)

	if got := h.imeActivatedCalls.Load(); got != 1 {
		t.Fatalf("HandleIMEActivated 应被调用 1 次, got=%d", got)
	}
	if got := h.lastIMEActivatedID.Load(); got != pid {
		t.Fatalf("HandleIMEActivated processID 不对; got=%d want=%d", got, pid)
	}

	// 从 outbound channel 拿到入队的消息, 验证 header + 关键字段。
	select {
	case msg := <-pc.outbound:
		if len(msg) < ipc.HeaderSize {
			t.Fatalf("push message 长度不足: %d", len(msg))
		}
		gotCmd := binary.LittleEndian.Uint16(msg[2:4])
		if gotCmd != ipc.CmdActivationStatusPush {
			t.Fatalf("push command 应为 CmdActivationStatusPush=0x%04X, got 0x%04X", ipc.CmdActivationStatusPush, gotCmd)
		}
		// StatusHeader at offset HeaderSize: flags(4) + keyDownCount(4) + keyUpCount(4)
		flags := binary.LittleEndian.Uint32(msg[ipc.HeaderSize : ipc.HeaderSize+4])
		if flags&ipc.StatusChineseMode == 0 {
			t.Fatalf("StatusChineseMode flag 应被设置, got flags=0x%08X", flags)
		}
		if flags&ipc.StatusToolbarVisible == 0 {
			t.Fatalf("StatusToolbarVisible flag 应被设置, got flags=0x%08X", flags)
		}
		keyDownCount := binary.LittleEndian.Uint32(msg[ipc.HeaderSize+4 : ipc.HeaderSize+8])
		if keyDownCount != 1 {
			t.Fatalf("keyDownCount 应为 1 (hotkeys 已传入), got=%d", keyDownCount)
		}
	case <-time.After(500 * time.Millisecond):
		t.Fatal("push 未在 500ms 内入队 outbound; runActivationHandlerAndPush 可能没调 PushActivationStatusToActiveClient")
	}
}

// TestRunActivationHandlerAndPush_FocusGainedEncodesAndPushes
// 同上但走 HandleFocusGained 分支。
func TestRunActivationHandlerAndPush_FocusGainedEncodesAndPushes(t *testing.T) {
	h := &fakeMessageHandler{
		focusGainedReturn: &StatusUpdateData{
			ChineseMode: false,
			IconLabel:   "英",
		},
	}
	s := newServerWithFakeHandler(t, h)

	const pid uint32 = 0xCAFE
	pc, _ := installFakePushClient(s, pid, 0)

	header := &ipc.IpcHeader{Command: ipc.CmdFocusGained}
	s.runActivationHandlerAndPush(header, nil, 1, pid)

	if got := h.focusGainedCalls.Load(); got != 1 {
		t.Fatalf("HandleFocusGained 应被调用 1 次, got=%d", got)
	}

	select {
	case msg := <-pc.outbound:
		gotCmd := binary.LittleEndian.Uint16(msg[2:4])
		if gotCmd != ipc.CmdActivationStatusPush {
			t.Fatalf("push command 应为 CmdActivationStatusPush, got 0x%04X", gotCmd)
		}
	case <-time.After(500 * time.Millisecond):
		t.Fatal("FocusGained push 未入队")
	}
}

// TestRunActivationHandlerAndPush_NilStatusDoesNotPush 守住契约 3:
// handler 返回 nil 时不得 push, 避免覆盖 C++ 端 mirror 的有效旧值。
func TestRunActivationHandlerAndPush_NilStatusDoesNotPush(t *testing.T) {
	h := &fakeMessageHandler{
		imeActivatedReturn: nil,
	}
	s := newServerWithFakeHandler(t, h)

	const pid uint32 = 0xDEAD
	pc, _ := installFakePushClient(s, pid, 0)

	header := &ipc.IpcHeader{Command: ipc.CmdIMEActivated}
	s.runActivationHandlerAndPush(header, nil, 1, pid)

	// handler 应当被调过 (验证不是因为路径完全没走), 但 outbound 应为空。
	if got := h.imeActivatedCalls.Load(); got != 1 {
		t.Fatalf("HandleIMEActivated 应被调用 1 次 (即使返回 nil), got=%d", got)
	}

	select {
	case msg := <-pc.outbound:
		t.Fatalf("handler 返回 nil 时不应 push, 但收到 %d 字节", len(msg))
	case <-time.After(100 * time.Millisecond):
		// 期望路径: 100ms 内没有任何入队。
	}
}
