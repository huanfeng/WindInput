package bridge

import (
	"context"
	"encoding/binary"
	"fmt"
	"runtime/debug"

	"github.com/huanfeng/wind_input/internal/ipc"
)

// processRequestWithTimeout wraps processRequest with a timeout
func (s *Server) processRequestWithTimeout(header *ipc.IpcHeader, payload []byte, clientID int, processID uint32) []byte {
	// 快速命令直接同步执行，避免 goroutine + channel 分配
	switch header.Command {
	case ipc.CmdFocusGained, ipc.CmdFocusLost, ipc.CmdIMEActivated,
		ipc.CmdCompositionTerminated, ipc.CmdCaretUpdate, ipc.CmdHostRenderRequest:
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

	// Update active process ID for events that indicate this client is active
	// This ensures activeProcessID is always current, even if FocusGained wasn't received yet
	switch header.Command {
	case ipc.CmdKeyEvent, ipc.CmdCommitRequest, ipc.CmdFocusGained, ipc.CmdIMEActivated, ipc.CmdCaretUpdate:
		if processID != 0 {
			s.activeMu.Lock()
			if s.activeProcessID != processID {
				s.logger.Info("Active process updated", "clientID", clientID, "oldProcessID", s.activeProcessID, "newProcessID", processID)
				s.activeProcessID = processID
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
		return s.handleFocusGained(payload, clientID, processID)

	case ipc.CmdFocusLost:
		s.handler.HandleFocusLost()
		return s.codec.EncodeAck()

	case ipc.CmdCompositionTerminated:
		s.logger.Debug("Composition unexpectedly terminated", "clientID", clientID)
		s.handler.HandleCompositionTerminated()
		return s.codec.EncodeAck()

	case ipc.CmdIMEActivated:
		s.logger.Info("IME activated (user switched back to this IME)", "clientID", clientID, "processID", processID)
		statusUpdate := s.handler.HandleIMEActivated()
		if statusUpdate != nil {
			return s.encodeStatusUpdateWithHostRender(statusUpdate, processID)
		}
		return s.codec.EncodeAck()

	case ipc.CmdIMEDeactivated:
		s.logger.Info("IME deactivated (user switched to another IME)", "clientID", clientID)
		s.handler.HandleIMEDeactivated()
		return s.codec.EncodeAck()

	case ipc.CmdModeNotify:
		return s.handleModeNotify(payload, clientID)

	case ipc.CmdToggleMode:
		return s.handleToggleMode(clientID)

	case ipc.CmdMenuCommand:
		return s.handleMenuCommand(payload, clientID)

	case ipc.CmdShowContextMenu:
		return s.handleShowContextMenu(payload, clientID)

	case ipc.CmdCaretUpdate:
		return s.handleCaretUpdate(payload, clientID)

	case ipc.CmdHostRenderRequest:
		return s.handleHostRenderRequest(clientID, processID)

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
	}

	s.logger.Debug("Key event", "clientID", clientID,
		"keyCode", keyData.KeyCode,
		"modifiers", fmt.Sprintf("0x%X", keyData.Modifiers),
		"toggles", fmt.Sprintf("0x%X", keyData.Toggles),
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
			"modeChanged", result.ModeChanged, "hasNewComposition", result.NewComposition != "")
		return s.codec.EncodeCommitText(result.Text, result.NewComposition, result.ModeChanged, result.ChineseMode)

	case ResponseTypeUpdateComposition:
		return s.codec.EncodeUpdateComposition(result.Text, result.CaretPos)

	case ResponseTypeClearComposition:
		return s.codec.EncodeClearComposition()

	case ResponseTypeModeChanged:
		s.logger.Debug("Returning ModeChanged response", "clientID", clientID, "chineseMode", result.ChineseMode)
		return s.codec.EncodeModeChanged(result.ChineseMode)

	case ResponseTypeConsumed:
		s.logger.Debug("Key consumed by hotkey", "clientID", clientID)
		return s.codec.EncodeConsumed()

	default:
		return s.codec.EncodeAck()
	}
}

func (s *Server) handleFocusGained(payload []byte, clientID int, processID uint32) []byte {
	// Note: activeProcessID is already updated in processRequest() for all relevant commands

	// Parse optional caret data
	if len(payload) >= 12 {
		caretPayload, err := s.codec.DecodeCaretPayload(payload)
		if err == nil {
			s.logger.Debug("Focus gained with caret", "x", caretPayload.X, "y", caretPayload.Y)
			s.handler.HandleCaretUpdate(CaretData{
				X:                 int(caretPayload.X),
				Y:                 int(caretPayload.Y),
				Height:            int(caretPayload.Height),
				CompositionStartX: int(caretPayload.CompositionStartX),
				CompositionStartY: int(caretPayload.CompositionStartY),
			})
		}
	}

	statusUpdate := s.handler.HandleFocusGained()
	if statusUpdate != nil {
		return s.encodeStatusUpdate(statusUpdate)
	}
	return s.codec.EncodeAck()
}

func (s *Server) handleCaretUpdate(payload []byte, clientID int) []byte {
	caretPayload, err := s.codec.DecodeCaretPayload(payload)
	if err != nil {
		s.logger.Error("Failed to decode caret payload", "clientID", clientID, "error", err)
		return s.codec.EncodeAck()
	}

	s.logger.Debug("Caret update", "clientID", clientID,
		"x", caretPayload.X, "y", caretPayload.Y, "height", caretPayload.Height)

	s.handler.HandleCaretUpdate(CaretData{
		X:                 int(caretPayload.X),
		Y:                 int(caretPayload.Y),
		Height:            int(caretPayload.Height),
		CompositionStartX: int(caretPayload.CompositionStartX),
		CompositionStartY: int(caretPayload.CompositionStartY),
	})

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

	// Call handler to toggle mode
	commitText, chineseMode := s.handler.HandleToggleMode()

	s.logger.Debug("Toggle mode result", "clientID", clientID,
		"chineseMode", chineseMode, "commitText", commitText)

	// Return ModeChanged response (with optional commit text if there was pending input)
	if commitText != "" {
		return s.codec.EncodeCommitText(commitText, "", true, chineseMode)
	}
	return s.codec.EncodeModeChanged(chineseMode)
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
func (s *Server) handleBatchEvents(header *ipc.IpcHeader, payload []byte, writer *pipeWriter, clientID int, processID uint32) {
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
		if err := s.codec.WriteMessage(writer, batchResponse); err != nil {
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

// encodeStatusUpdateWithHostRender encodes a status update, adding the HOST_RENDER_AVAIL flag
// if the process is whitelisted for host rendering.
func (s *Server) encodeStatusUpdateWithHostRender(status *StatusUpdateData, processID uint32) []byte {
	hostRenderAvail := false
	if s.hostRender != nil && processID != 0 {
		hostRenderAvail = s.hostRender.IsProcessWhitelisted(processID)
	}
	return s.codec.EncodeStatusUpdateEx(
		status.ChineseMode,
		status.FullWidth,
		status.ChinesePunctuation,
		status.ToolbarVisible,
		status.CapsLock,
		hostRenderAvail,
		status.KeyDownHotkeys,
		status.KeyUpHotkeys,
		status.IconLabel,
	)
}

// handleHostRenderRequest handles CmdHostRenderRequest from DLL
func (s *Server) handleHostRenderRequest(clientID int, processID uint32) []byte {
	if s.hostRender == nil || processID == 0 {
		s.logger.Warn("Host render request rejected: no manager or no PID", "clientID", clientID)
		return s.codec.EncodeAck()
	}

	setup, err := s.hostRender.SetupHostRender(processID)
	if err != nil {
		s.logger.Error("Failed to setup host render", "clientID", clientID, "processID", processID, "error", err)
		return s.codec.EncodeAck()
	}

	s.logger.Info("Host render setup sent", "clientID", clientID, "processID", processID,
		"shmName", setup.ShmName, "eventName", setup.EventName)

	// Notify coordinator that host render is ready so it can update UI render callbacks.
	// This handles the case where FocusGained arrived before host render was set up
	// (e.g., during first activation when OnSetFocus fires before _DoFullStateSync).
	s.handler.HandleHostRenderReady()

	return s.codec.EncodeHostRenderSetup(setup)
}

// keyCodeToKeyName converts a virtual key code to a key name string
// This is for backwards compatibility with the existing handler interface
func keyCodeToKeyName(keyCode uint32) string {
	switch keyCode {
	case ipc.VK_BACK:
		return "backspace"
	case ipc.VK_TAB:
		return "tab"
	case ipc.VK_RETURN:
		return "enter"
	case ipc.VK_ESCAPE:
		return "escape"
	case ipc.VK_SPACE:
		return "space"
	case ipc.VK_PRIOR:
		return "page_up"
	case ipc.VK_NEXT:
		return "page_down"
	case ipc.VK_CAPITAL:
		return "capslock"
	case ipc.VK_LSHIFT:
		return "lshift"
	case ipc.VK_RSHIFT:
		return "rshift"
	case ipc.VK_LCONTROL:
		return "lctrl"
	case ipc.VK_RCONTROL:
		return "rctrl"
	case ipc.VK_OEM_1:
		return ";"
	case ipc.VK_OEM_PLUS:
		return "="
	case ipc.VK_OEM_COMMA:
		return ","
	case ipc.VK_OEM_MINUS:
		return "-"
	case ipc.VK_OEM_PERIOD:
		return "."
	case ipc.VK_OEM_2:
		return "/"
	case ipc.VK_OEM_3:
		return "`"
	case ipc.VK_OEM_4:
		return "["
	case ipc.VK_OEM_5:
		return "\\"
	case ipc.VK_OEM_6:
		return "]"
	case ipc.VK_OEM_7:
		return "'"
	default:
		// Letters A-Z
		if keyCode >= 0x41 && keyCode <= 0x5A {
			return string(rune('a' + keyCode - 0x41))
		}
		// Numbers 0-9
		if keyCode >= 0x30 && keyCode <= 0x39 {
			return string(rune('0' + keyCode - 0x30))
		}
		// Numpad 0-9
		if keyCode >= 0x60 && keyCode <= 0x69 {
			return string(rune('0' + keyCode - 0x60))
		}
		// Numpad operators
		switch keyCode {
		case 0x6A: // VK_MULTIPLY
			return "*"
		case 0x6B: // VK_ADD
			return "+"
		case 0x6D: // VK_SUBTRACT
			return "-"
		case 0x6E: // VK_DECIMAL
			return "."
		case 0x6F: // VK_DIVIDE
			return "/"
		}
		return fmt.Sprintf("vk_%d", keyCode)
	}
}
