//go:build windows

package bridge

import (
	"context"
	"encoding/binary"
	"fmt"
	"io"
	"runtime/debug"
	"time"

	"github.com/huanfeng/wind_input/internal/ipc"
)

// slowRequestThreshold 是同步 bridge 请求处理时长的警戒线。
// 超过即写 WARN，便于排查「宿主 UI 线程被同步 IPC 阻塞」这类问题
// （例如 IME activate 同步路径误调用了跨进程 shell API，导致 C++ 端
// READ_TIMEOUT_MS=1500ms 命中超时、host explorer 卡 1.5s）。
//
// 20ms 是异步化后的金丝雀：IMEActivated/FocusGained 异步化后 processRequest 只做
// 字段更新 + 立即回 Ack，正常 <1ms；剩余的 KeyEvent/CommitRequest 也几乎全走 mmap
// 查询，正常 <5ms。20ms 命中说明引入了不该有的慢调用，必须排查。
// 历史阈值 50ms 在异步化前能稳定不误报，但异步化后偏松。
const slowRequestThreshold = 20 * time.Millisecond

// slowActivationThreshold 是 activation 第二段 (handler + push 入队) 时长的警戒线。
// 触发 WARN 暴露两类问题:
//  1. HandleIMEActivated / HandleFocusGained 自身变慢 (新引入的耗时调用、锁竞争);
//  2. push pipe outbound 队列堆积导致 enqueue 退化为非 O(1) 路径。
//
// 100ms 是体感门槛: activation 拖到 100ms 以上, 用户切应用 / Ctrl+Space 的
// 工具栏出现会肉眼可见地延迟。设这条警戒线让"切应用就慢"提前被发现。
const slowActivationThreshold = 100 * time.Millisecond

// processRequestWithTimeout wraps processRequest with a timeout
func (s *Server) processRequestWithTimeout(header *ipc.IpcHeader, payload []byte, clientID int, processID uint32) []byte {
	t0 := time.Now()
	defer func() {
		if d := time.Since(t0); d > slowRequestThreshold {
			s.logger.Warn("Slow bridge request",
				"command", fmt.Sprintf("0x%04X", header.Command),
				"duration", d,
				"clientID", clientID,
				"processID", processID)
		}
	}()

	// 快速命令直接同步执行，避免 goroutine + channel 分配。
	// CmdKeyEvent/CmdCommitRequest 是最高频命令，词典查询几乎全走 mmap，耗时远低于 200ms 超时。
	switch header.Command {
	case ipc.CmdKeyEvent, ipc.CmdCommitRequest,
		ipc.CmdFocusGained, ipc.CmdFocusLost, ipc.CmdIMEActivated,
		ipc.CmdCompositionTerminated, ipc.CmdCaretUpdate, ipc.CmdCaretPending, ipc.CmdHostRenderRequest,
		ipc.CmdCandidateSelect, ipc.CmdCandidateHover, ipc.CmdCandidateScroll:
		// host render 鼠标事件：DLL 走 SendAsync（不等响应），仅做轻量分发到
		// coordinator goroutine，必须留在同步快速路径，绝不能进 goroutine+timeout。
		return s.processRequest(header, payload, clientID, processID)
	}

	// 耗时命令（如按键处理）仍使用 goroutine + timeout
	ctx, cancel := context.WithTimeout(context.Background(), RequestProcessTimeout)
	defer cancel()

	resultCh := make(chan []byte, 1)

	go func() {
		resultCh <- s.processRequest(header, payload, clientID, processID)
	}()

	select {
	case response := <-resultCh:
		return response
	case <-ctx.Done():
		s.logger.Error("Request processing timed out", "clientID", clientID, "command", header.Command)
		return s.codec.EncodeAck()
	}
}

func (s *Server) processRequest(header *ipc.IpcHeader, payload []byte, clientID int, processID uint32) (resp []byte) {
	defer func() {
		if r := recover(); r != nil {
			s.logger.Error("PANIC in processRequest", "clientID", clientID, "command", fmt.Sprintf("0x%04X", header.Command), "panic", fmt.Sprintf("%v", r), "stack", string(debug.Stack()))
			resp = s.codec.EncodeAck()
		}
	}()
	s.logger.Debug("Processing Bridge request", "clientID", clientID, "command", fmt.Sprintf("0x%04X", header.Command))

	// Update active process ID for events that indicate this client is active.
	// CMD_IME_ACTIVATED / CMD_FOCUS_GAINED also carry a per-instance token that
	// allows precise push targeting within multi-instance hosts (e.g. explorer).
	// Token format: ((uint64)PID << 32) | per-process-instance-counter（见 IPCClient _clientToken）
	switch header.Command {
	case ipc.CmdKeyEvent, ipc.CmdCommitRequest, ipc.CmdFocusGained, ipc.CmdIMEActivated, ipc.CmdCaretUpdate:
		if processID != 0 {
			s.activeMu.Lock()
			if s.activeProcessID != processID {
				s.logger.Info("Active process updated", "clientID", clientID, "oldProcessID", s.activeProcessID, "newProcessID", processID)
				s.activeProcessID = processID
			}
			// Extract client token only from IME_ACTIVATED (payload = 8-byte token).
			// FOCUS_GAINED intentionally excluded: two-process TSF apps (e.g. EverEdit)
			// use Process A for key events (FOCUS_GAINED) and Process B for push pipe
			// (IME_ACTIVATED). Overwriting activeToken with Process A's token would break
			// push-pipe targeting since Process A never connects to the push pipe.
			var token uint64
			if header.Command == ipc.CmdIMEActivated && len(payload) >= 8 {
				token = binary.LittleEndian.Uint64(payload[:8])
			}
			if token != 0 && token != s.activeToken {
				s.logger.Debug("Active token updated", "clientID", clientID, "token", token)
				s.activeToken = token
			}
			s.activeMu.Unlock()
		}
	}

	switch header.Command {
	case ipc.CmdKeyEvent:
		return s.handleKeyEvent(payload, clientID)

	case ipc.CmdCommitRequest:
		return s.handleCommitRequest(payload, clientID)

	case ipc.CmdFocusGained:
		// 仅当 DLL 真正向 Go 投递 FOCUS_GAINED 时才把 (clientID,PID) 标为"有焦点"。
		// DLL 在 OnSetFocus 里已经过 _hasTextInputContext / XamlIsland gate
		// 过滤掉无文本输入上下文的 DocMgr，所以这里到达即可信。
		s.markFocused(clientID, processID)
		// 异步化第一段：只做 caret 字段同步（纯字段写入，第一次按键前必须就绪）+ 立即回 Ack。
		// 真正的 HandleFocusGained 由 handleClient 在 Ack 已写出后内联触发，状态走 push pipe。
		s.applyFocusGainedCaret(payload, clientID)
		// 模式预推送：在回 Ack 前入队 CmdModePush，仅携带 chineseMode+fullWidth。
		// 使 DLL 侧模式就绪时机从激活 push（~15ms）提前至 ~1ms，消除首次按键竞态窗口。
		// GetCurrentMode 极轻量（锁+读两字段），PushModePushToActiveClient 为非阻塞入队。
		chineseMode, fullWidth := s.handler.GetCurrentMode()
		s.PushModePushToActiveClient(chineseMode, fullWidth)
		return s.codec.EncodeAck()

	case ipc.CmdFocusLost:
		s.markUnfocused(clientID)
		s.handler.HandleFocusLost()
		return s.codec.EncodeAck()

	case ipc.CmdCompositionTerminated:
		s.logger.Debug("Composition unexpectedly terminated", "clientID", clientID)
		s.handler.HandleCompositionTerminated()
		return s.codec.EncodeAck()

	case ipc.CmdIMEActivated:
		s.logger.Info("IME activated (user switched back to this IME)", "clientID", clientID, "processID", processID)
		s.markFocused(clientID, processID)
		// 异步化（见 bridge/AGENTS.md 红线条款）：本 case 仅做 active 状态字段更新；
		// 真正的 HandleIMEActivated 调用由 handleClient 在 Ack 已写出后内联触发
		// （见 handleClient 中 isActivationCommand 分支），通过 PushActivationStatusToActiveClient
		// 把状态以 CmdActivationStatusPush 回送 C++ 端。
		// 进入此 case 时 activeProcessID / activeToken 已在 processRequest 顶部完成更新。
		return s.codec.EncodeAck()

	case ipc.CmdIMEDeactivated:
		s.logger.Info("IME deactivated (user switched to another IME)", "clientID", clientID)
		// 用户切到别的输入法时，本实例进入 Deactivated 状态。仅摘掉本 clientID
		// 那一条记录；同 PID 的其它实例（如 Notepad11 多 tab 的另一条 TextService）
		// 不受影响。这条记录的删除保证了：若所有实例都 Deactivate（用户全局切
		// 走我们的 IME），focusedClients 中该 PID 自然清空，hook 不再激活；
		// 若只是单实例退出（如关 tab），其它实例的记录会保留 PID 在 hook 视角的活跃性。
		s.markUnfocused(clientID)
		s.handler.HandleIMEDeactivated()
		return s.codec.EncodeAck()

	case ipc.CmdModeNotify:
		return s.handleModeNotify(payload, clientID)

	case ipc.CmdToggleMode:
		return s.handleToggleMode(clientID)

	case ipc.CmdSystemModeSwitch:
		return s.handleSystemModeSwitch(payload, clientID)

	case ipc.CmdMenuCommand:
		return s.handleMenuCommand(payload, clientID)

	case ipc.CmdShowContextMenu:
		return s.handleShowContextMenu(payload, clientID)

	case ipc.CmdCaretUpdate:
		return s.handleCaretUpdate(payload, clientID)

	case ipc.CmdCaretPending:
		s.logger.Debug("Caret pending (composition just started, awaiting reflow)", "clientID", clientID)
		s.handler.HandleCaretPending()
		return s.codec.EncodeAck()

	case ipc.CmdSelectionChanged:
		return s.handleSelectionChanged(payload, clientID)

	case ipc.CmdHostRenderRequest:
		return s.handleHostRenderRequest(clientID, processID)

	case ipc.CmdCandidateSelect:
		return s.handleHostCandidateSelect(payload)

	case ipc.CmdCandidateHover:
		return s.handleHostCandidateHover(payload)

	case ipc.CmdCandidateScroll:
		return s.handleHostCandidateScroll(payload)

	case ipc.CmdInputStats:
		return s.handleInputStats(payload, clientID)

	default:
		s.logger.Error("Unknown command from Bridge", "clientID", clientID, "command", fmt.Sprintf("0x%04X", header.Command))
		return s.codec.EncodeAck()
	}
}

func (s *Server) handleKeyEvent(payload []byte, clientID int) []byte {
	keyPayload, err := s.codec.DecodeKeyPayload(payload)
	if err != nil {
		s.logger.Error("Failed to decode key payload", "clientID", clientID, "error", err)
		return s.codec.EncodeAck()
	}

	// Convert to KeyEventData
	eventType := "down"
	if keyPayload.EventType == ipc.KeyEventUp {
		eventType = "up"
	}

	keyData := KeyEventData{
		Key:       keyCodeToKeyName(keyPayload.KeyCode),
		KeyCode:   int(keyPayload.KeyCode),
		Modifiers: int(keyPayload.Modifiers),
		Event:     eventType,
		Toggles:   keyPayload.Toggles,
		PrevChar:  rune(keyPayload.PrevChar),
	}

	s.logger.Debug("Key event", "clientID", clientID,
		"keyCode", keyData.KeyCode,
		"modifiers", fmt.Sprintf("0x%X", keyData.Modifiers),
		"toggles", fmt.Sprintf("0x%X", keyData.Toggles),
		"prevChar", fmt.Sprintf("%d(%s)", keyData.PrevChar, string(keyData.PrevChar)),
		"event", eventType)

	result := s.handler.HandleKeyEvent(keyData)
	if result == nil {
		// Key not handled by IME, tell C++ to pass it through to the system
		s.logger.Debug("Returning PassThrough response", "clientID", clientID)
		return s.codec.EncodePassThrough()
	}

	// Build response based on result
	switch result.Type {
	case ResponseTypeInsertText:
		s.logger.Debug("Returning CommitText response", "clientID", clientID,
			"modeChanged", result.ModeChanged, "hasNewComposition", result.HasNewComposition)
		return s.codec.EncodeCommitText(result.Text, result.NewComposition, result.ModeChanged, result.ChineseMode, result.HasNewComposition)

	case ResponseTypeUpdateComposition:
		return s.codec.EncodeUpdateComposition(result.Text, result.CaretPos)

	case ResponseTypeClearComposition:
		return s.codec.EncodeClearComposition()

	case ResponseTypeStatusUpdate:
		// 模式切换走这条：自包含 iconLabel，C++ 端 StatusUpdate handler 立刻
		// UpdateFullStatus → 刷新任务栏图标，不依赖 push pipe。
		if result.Status == nil {
			s.logger.Error("StatusUpdate response missing Status payload", "clientID", clientID)
			return s.codec.EncodeAck()
		}
		s.logger.Debug("Returning StatusUpdate response", "clientID", clientID,
			"chineseMode", result.Status.ChineseMode, "iconLabel", result.Status.IconLabel)
		return s.encodeStatusUpdate(result.Status)

	case ResponseTypeConsumed:
		s.logger.Debug("Key consumed by hotkey", "clientID", clientID)
		return s.codec.EncodeConsumed()

	case ResponseTypeInsertTextWithCursor:
		s.logger.Debug("Returning CommitTextWithCursor response", "clientID", clientID,
			"cursorOffset", result.CursorOffset)
		return s.codec.EncodeCommitTextWithCursor(result.Text, result.CursorOffset)

	case ResponseTypeMoveCursorRight:
		s.logger.Debug("Returning MoveCursorRight response", "clientID", clientID)
		return s.codec.EncodeMoveCursor(1)

	case ResponseTypeDeletePair:
		s.logger.Debug("Returning DeletePair response", "clientID", clientID)
		return s.codec.EncodeDeletePair()

	default:
		return s.codec.EncodeAck()
	}
}

// applyFocusGainedCaret 在 CmdFocusGained 同步路径上仅做 caret 字段写入（无 shell 调用）。
// 与 HandleFocusGained 不同：HandleCaretUpdate 是纯字段同步，第一次按键的工具栏定位依赖它，
// 因此保留在 Ack 之前；HandleFocusGained 可能触发工具栏显示/状态聚合，搬到 Ack 之后。
func (s *Server) applyFocusGainedCaret(payload []byte, clientID int) {
	if len(payload) < 12 {
		return
	}
	caretPayload, err := s.codec.DecodeCaretPayload(payload)
	if err != nil {
		return
	}
	s.logger.Debug("Focus gained with caret", "clientID", clientID,
		"x", caretPayload.X, "y", caretPayload.Y)
	s.handler.HandleCaretUpdate(CaretData{
		X:                 int(caretPayload.X),
		Y:                 int(caretPayload.Y),
		Height:            int(caretPayload.Height),
		CompositionStartX: int(caretPayload.CompositionStartX),
		CompositionStartY: int(caretPayload.CompositionStartY),
	})
}

// runActivationHandlerAndPush 是 IMEActivated / FocusGained 异步化的第二段（见 server.go::handleClient）：
// Ack 已写出后，在 handleClient 同 goroutine 内做真正的 handler 调用，结果通过 push pipe
// 以 CmdActivationStatusPush 回送 C++ 端。
//
// 留在 handleClient 同 goroutine（而非 spawn 新 goroutine）有两个理由：
//  1. 同 client 上后续的 IMEDeactivated / FocusLost 必须排在本次 activation 之后处理，
//     单 goroutine 串行天然保证顺序，免去额外锁。
//  2. C++ 端已经收到 Ack 后可继续派发新命令；它们会在 handleClient 下一轮 ReadHeader
//     时排队读取，对 C++ 端体感无影响。
func (s *Server) runActivationHandlerAndPush(header *ipc.IpcHeader, payload []byte, clientID int, processID uint32) {
	t0 := time.Now()
	defer func() {
		if r := recover(); r != nil {
			s.logger.Error("PANIC in runActivationHandlerAndPush",
				"clientID", clientID, "command", fmt.Sprintf("0x%04X", header.Command),
				"panic", fmt.Sprintf("%v", r), "stack", string(debug.Stack()))
			return
		}
		if d := time.Since(t0); d > slowActivationThreshold {
			s.logger.Warn("Slow activation handler",
				"command", fmt.Sprintf("0x%04X", header.Command),
				"duration", d,
				"clientID", clientID,
				"processID", processID)
		}
	}()

	var status *StatusUpdateData
	switch header.Command {
	case ipc.CmdIMEActivated:
		status = s.handler.HandleIMEActivated(processID)
	case ipc.CmdFocusGained:
		// payload 即原 FocusGained 命令载荷（与 processRequest 同一 goroutine 串行，未被复用）。
		// 末尾 8 字节是 InputScope bitmask，用于密码框强制英文等决策。
		inputScopeMask := s.codec.DecodeFocusGainedInputScope(payload)
		status = s.handler.HandleFocusGained(processID, inputScopeMask)
	default:
		s.logger.Error("runActivationHandlerAndPush: unexpected command", "command", fmt.Sprintf("0x%04X", header.Command))
		return
	}

	if status == nil {
		s.logger.Debug("Activation handler returned nil status, skipping push", "clientID", clientID, "command", fmt.Sprintf("0x%04X", header.Command))
		return
	}

	s.PushActivationStatusToActiveClient(status, processID)
}

func (s *Server) handleCaretUpdate(payload []byte, clientID int) []byte {
	caretPayload, err := s.codec.DecodeCaretPayload(payload)
	if err != nil {
		s.logger.Error("Failed to decode caret payload", "clientID", clientID, "error", err)
		return s.codec.EncodeAck()
	}

	s.logger.Debug("Caret update", "clientID", clientID,
		"x", caretPayload.X, "y", caretPayload.Y, "height", caretPayload.Height,
		"compStartX", caretPayload.CompositionStartX, "compStartY", caretPayload.CompositionStartY)

	s.handler.HandleCaretUpdate(CaretData{
		X:                 int(caretPayload.X),
		Y:                 int(caretPayload.Y),
		Height:            int(caretPayload.Height),
		CompositionStartX: int(caretPayload.CompositionStartX),
		CompositionStartY: int(caretPayload.CompositionStartY),
	})

	return s.codec.EncodeAck()
}

func (s *Server) handleSelectionChanged(payload []byte, clientID int) []byte {
	var prevChar rune
	if len(payload) >= 4 {
		prevChar = rune(binary.LittleEndian.Uint16(payload[0:2]))
	}

	s.logger.Debug("Selection changed", "clientID", clientID, "prevChar", prevChar)
	s.handler.HandleSelectionChanged(prevChar)

	return s.codec.EncodeAck()
}

func (s *Server) handleShowContextMenu(payload []byte, clientID int) []byte {
	if len(payload) < 8 {
		s.logger.Error("ShowContextMenu payload too short", "clientID", clientID)
		return s.codec.EncodeAck()
	}

	screenX := int(int32(binary.LittleEndian.Uint32(payload[0:4])))
	screenY := int(int32(binary.LittleEndian.Uint32(payload[4:8])))

	s.logger.Info("ShowContextMenu request from TSF", "clientID", clientID,
		"screenX", screenX, "screenY", screenY)

	s.handler.HandleShowContextMenu(screenX, screenY)
	return s.codec.EncodeAck()
}

func (s *Server) handleCommitRequest(payload []byte, clientID int) []byte {
	commitReq, err := s.codec.DecodeCommitRequestPayload(payload)
	if err != nil {
		s.logger.Error("Failed to decode commit request payload", "clientID", clientID, "error", err)
		return s.codec.EncodeAck()
	}

	s.logger.Debug("Commit request", "clientID", clientID,
		"barrierSeq", commitReq.BarrierSeq,
		"triggerKey", fmt.Sprintf("0x%04X", commitReq.TriggerKey),
		"inputBuffer", commitReq.InputBuffer)

	// Convert to CommitRequestData
	reqData := CommitRequestData{
		BarrierSeq:  commitReq.BarrierSeq,
		TriggerKey:  commitReq.TriggerKey,
		Modifiers:   commitReq.Modifiers,
		InputBuffer: commitReq.InputBuffer,
	}

	// Handle the commit request
	result := s.handler.HandleCommitRequest(reqData)
	if result == nil {
		// No result, return ACK
		return s.codec.EncodeAck()
	}

	// Encode and return commit result
	return s.codec.EncodeCommitResult(
		result.BarrierSeq,
		result.Text,
		result.NewComposition,
		result.ModeChanged,
		result.ChineseMode,
	)
}

func (s *Server) handleToggleMode(clientID int) []byte {
	s.logger.Info("Toggle mode request from UI", "clientID", clientID)

	// 统一架构：Go 决定最终状态后以 StatusUpdate 回应，C++ 端走 UpdateFullStatus
	// 一并同步内部 mirror + TSF compartments + LangBar UI。
	status, commitText := s.handler.HandleToggleMode()

	s.logger.Debug("Toggle mode result", "clientID", clientID,
		"hasCommit", commitText != "", "hasStatus", status != nil)

	// commitText 路径仍走 CommitText（带 ModeChanged bit）；后续 push pipe 会推送
	// 完整 status，C++ 端 LangBar 一致性由 push 路径保障。
	if commitText != "" {
		chineseMode := false
		if status != nil {
			chineseMode = status.ChineseMode
		}
		return s.codec.EncodeCommitText(commitText, "", true, chineseMode, false)
	}
	if status == nil {
		return s.codec.EncodeAck()
	}
	return s.encodeStatusUpdate(status)
}

func (s *Server) handleSystemModeSwitch(payload []byte, clientID int) []byte {
	if len(payload) < 4 {
		s.logger.Error("System mode switch payload too short", "clientID", clientID)
		return s.codec.EncodeAck()
	}

	// Parse flags (same format as StatusFlags). 注意：这是系统已经决定好的目标模式，
	// Go 必须 follow 而非 toggle。
	flags := binary.LittleEndian.Uint32(payload[0:4])
	chineseMode := (flags & ipc.StatusChineseMode) != 0

	s.logger.Info("System mode switch", "clientID", clientID, "targetMode", chineseMode)

	status, commitText := s.handler.HandleSystemModeSwitch(chineseMode)

	s.logger.Debug("System mode switch result", "clientID", clientID,
		"chineseMode", chineseMode, "hasCommit", commitText != "")

	if commitText != "" {
		return s.codec.EncodeCommitText(commitText, "", true, chineseMode, false)
	}
	if status == nil {
		return s.codec.EncodeAck()
	}
	return s.encodeStatusUpdate(status)
}

func (s *Server) handleMenuCommand(payload []byte, clientID int) []byte {
	// Payload is UTF-8 encoded command string
	command := string(payload)
	s.logger.Info("Menu command from TSF", "clientID", clientID, "command", command)

	// Call handler to process menu command
	statusUpdate := s.handler.HandleMenuCommand(command)

	if statusUpdate != nil {
		return s.encodeStatusUpdate(statusUpdate)
	}
	return s.codec.EncodeAck()
}

func (s *Server) handleModeNotify(payload []byte, clientID int) []byte {
	if len(payload) < 4 {
		s.logger.Error("Mode notify payload too short", "clientID", clientID)
		return s.codec.EncodeAck()
	}

	// Parse flags (same format as StatusFlags)
	flags := binary.LittleEndian.Uint32(payload[0:4])
	chineseMode := (flags & ipc.StatusChineseMode) != 0
	clearInput := (flags & ipc.StatusModeChanged) != 0

	s.logger.Info("Mode notify from TSF", "clientID", clientID,
		"chineseMode", chineseMode, "clearInput", clearInput)

	// Notify handler (async, no response needed)
	s.handler.HandleModeNotify(ModeNotifyData{
		ChineseMode: chineseMode,
		ClearInput:  clearInput,
	})

	return s.codec.EncodeAck()
}

// handleBatchEvents processes a batch of events and sends responses for sync events only
func (s *Server) handleBatchEvents(header *ipc.IpcHeader, payload []byte, w io.Writer, clientID int, processID uint32) {
	events, err := s.codec.DecodeBatchEvents(payload)
	if err != nil {
		s.logger.Error("Failed to decode batch events", "clientID", clientID, "error", err)
		return
	}

	s.logger.Debug("Processing batch events", "clientID", clientID, "count", len(events))

	// Collect responses for sync events
	var responses [][]byte

	for i, event := range events {
		// Process each event
		response := s.processRequestWithTimeout(event.Header, event.Payload, clientID, processID)

		// Only collect responses for sync events
		if !event.IsAsync {
			responses = append(responses, response)
			s.logger.Debug("Batch event sync", "clientID", clientID, "index", i, "command", fmt.Sprintf("0x%04X", event.Header.Command))
		} else {
			s.logger.Debug("Batch event async", "clientID", clientID, "index", i, "command", fmt.Sprintf("0x%04X", event.Header.Command))
		}
	}

	// Send batch response if there are any sync events
	if len(responses) > 0 {
		batchResponse := s.codec.EncodeBatchResponse(responses)
		if err := s.codec.WriteMessage(w, batchResponse); err != nil {
			s.logger.Error("Failed to write batch response to Bridge", "clientID", clientID, "error", err)
		}
	}
}

func (s *Server) encodeStatusUpdate(status *StatusUpdateData) []byte {
	return s.codec.EncodeStatusUpdate(
		status.ChineseMode,
		status.FullWidth,
		status.ChinesePunctuation,
		status.ToolbarVisible,
		status.CapsLock,
		status.KeyDownHotkeys,
		status.KeyUpHotkeys,
		status.IconLabel,
	)
}

// 注：原 encodeStatusUpdateWithHostRender 已移除。仅由 IMEActivated / FocusGained 的同步
// 响应路径使用过，异步化后状态走 PushActivationStatusToActiveClient → EncodeActivationStatusPush。
// EncodeStatusUpdateEx 仍由 PushActivationStatusToActiveClient 间接使用（在 server_push.go 中）。

// handleHostRenderRequest handles CmdHostRenderRequest from DLL
func (s *Server) handleHostRenderRequest(clientID int, processID uint32) []byte {
	if s.hostRender == nil || processID == 0 {
		s.logger.Warn("Host render request rejected: no manager or no PID", "clientID", clientID)
		return s.codec.EncodeAck()
	}
	if !s.hostRender.IsProcessWhitelisted(processID) {
		s.logger.Warn("Host render request rejected: process not whitelisted", "clientID", clientID, "processID", processID)
		return s.codec.EncodeAck()
	}

	setup, err := s.hostRender.SetupHostRender(processID)
	if err != nil {
		s.logger.Error("Failed to setup host render", "clientID", clientID, "processID", processID, "error", err)
		return s.codec.EncodeAck()
	}

	s.logger.Info("Host render setup sent", "clientID", clientID, "processID", processID,
		"kinds", len(setup))

	// Notify coordinator that host render is ready so it can update UI render callbacks.
	// This handles the case where FocusGained arrived before host render was set up
	// (e.g., during first activation when OnSetFocus fires before _DoFullStateSync).
	s.handler.HandleHostRenderReady()

	return s.codec.EncodeHostRenderSetup(setup)
}

// handleHostCandidateSelect 处理 host render 宿主窗口的鼠标左键点选（DLL 经 CmdCandidateSelect
// 异步上报，无响应写回）。payload = pageLocalIndex i32；负值是翻页按钮（-1=上页 -2=下页），
// 与 SHM 内嵌命中矩形的约定一致。经可选接口 candidateSelector 派发到 Coordinator，
// 选词/翻页逻辑与本地候选窗完全一致；结果走 push 管道。
func (s *Server) handleHostCandidateSelect(payload []byte) []byte {
	if len(payload) >= 4 {
		idx := int(int32(binary.LittleEndian.Uint32(payload[0:4])))
		cs, ok := s.handler.(candidateSelector)
		s.logger.Info("Host render candidate select", "index", idx, "routed", ok)
		if ok {
			cs.HandleCandidateSelect(idx)
		}
	}
	return s.codec.EncodeAck()
}

// handleHostCandidateScroll 处理 host render 候选框的鼠标滚轮（DLL 经 CmdCandidateScroll
// 异步上报，无响应写回）。payload = delta i32（WHEEL_DELTA 倍数，正=上滚）。经可选接口
// candidateScrollHandler 派发到 Coordinator 统一决策（默认不翻页）。
func (s *Server) handleHostCandidateScroll(payload []byte) []byte {
	if len(payload) >= 4 {
		delta := int(int32(binary.LittleEndian.Uint32(payload[0:4])))
		if h, ok := s.handler.(candidateScrollHandler); ok {
			h.HandleCandidateScroll(delta)
		}
	}
	return s.codec.EncodeAck()
}

// handleHostCandidateHover 处理 host render 宿主窗口的鼠标悬停（DLL 经 CmdCandidateHover
// 异步上报，无响应写回）。payload = index i32 + anchorX i32 + belowY i32 + aboveY i32：
// 屏幕锚点由 DLL 据 host 窗口屏幕位置 + 悬停候选矩形算出，Go 端据此定位 tooltip。
// 经可选接口 hostCandidateHoverHandler 派发到 Coordinator：触发带高亮的重渲染（经 SHM
// 重推宿主帧）+ tooltip 异步查询。index<0 表示离开候选区。
func (s *Server) handleHostCandidateHover(payload []byte) []byte {
	if len(payload) >= 4 {
		idx := int(int32(binary.LittleEndian.Uint32(payload[0:4])))
		tooltipX, belowY, aboveY := 0, 0, 0
		if len(payload) >= 16 {
			tooltipX = int(int32(binary.LittleEndian.Uint32(payload[4:8])))
			belowY = int(int32(binary.LittleEndian.Uint32(payload[8:12])))
			aboveY = int(int32(binary.LittleEndian.Uint32(payload[12:16])))
		}
		if h, ok := s.handler.(hostCandidateHoverHandler); ok {
			h.HandleCandidateHoverAt(idx, tooltipX, belowY, aboveY)
		}
	}
	return s.codec.EncodeAck()
}

// handleInputStats 处理 TSF 英文模式的输入统计上报（异步，无需响应）
func (s *Server) handleInputStats(payload []byte, clientID int) []byte {
	if len(payload) < 20 {
		s.logger.Error("InputStats payload too short", "clientID", clientID, "len", len(payload))
		return s.codec.EncodeAck()
	}
	chars := int(binary.LittleEndian.Uint32(payload[0:4]))
	digits := int(binary.LittleEndian.Uint32(payload[4:8]))
	puncts := int(binary.LittleEndian.Uint32(payload[8:12]))
	spaces := int(binary.LittleEndian.Uint32(payload[12:16]))
	elapsedMs := int(binary.LittleEndian.Uint32(payload[16:20]))
	s.logger.Debug("InputStats received from TSF English mode", "clientID", clientID,
		"chars", chars, "digits", digits, "puncts", puncts, "spaces", spaces, "elapsedMs", elapsedMs, "total", chars+digits+puncts+spaces)
	s.handler.HandleInputStats(chars, digits, puncts, spaces, elapsedMs)
	return s.codec.EncodeAck()
}
