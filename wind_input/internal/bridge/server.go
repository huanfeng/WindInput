// Package bridge handles IPC communication with C++ TSF Bridge
package bridge

import (
	"context"
	"encoding/binary"
	"fmt"
	"io"
	"log/slog"
	"sync"
	"time"
	"unsafe"

	"github.com/huanfeng/wind_input/internal/ipc"
	"golang.org/x/sys/windows"
)

const (
	BridgePipeName = `\\.\pipe\wind_input`
	PushPipeName   = `\\.\pipe\wind_input_push`

	// Buffer size for named pipe (64KB like Weasel)
	PipeBufferSize = 64 * 1024

	// Timeout for processing a single request
	RequestProcessTimeout = 200 * time.Millisecond
)

// Server handles IPC communication with C++ TSF Bridge
type Server struct {
	logger  *slog.Logger
	handler MessageHandler
	codec   *ipc.BinaryCodec

	mu            sync.RWMutex
	clientCount   int
	activeHandles map[windows.Handle]*pipeWriter // Map handle to writer for broadcasting

	// Push pipe clients (for proactive state push)
	pushMu          sync.RWMutex
	pushClientCount int
	pushClients     map[windows.Handle]*pipeWriter
}

// NewServer creates a new Bridge IPC server
func NewServer(handler MessageHandler, logger *slog.Logger) *Server {
	return &Server{
		handler:       handler,
		logger:        logger,
		codec:         ipc.NewBinaryCodec(),
		activeHandles: make(map[windows.Handle]*pipeWriter),
		pushClients:   make(map[windows.Handle]*pipeWriter),
	}
}

// Start begins listening for connections from C++ Bridge
func (s *Server) Start() error {
	s.logger.Info("Starting Bridge IPC server (binary protocol)", "pipe", BridgePipeName)

	// Start the push pipe listener in a separate goroutine
	go s.startPushPipeListener()

	// Create security descriptor allowing Everyone, SYSTEM, and Administrators
	sddl := "D:P(A;;GA;;;WD)(A;;GA;;;SY)(A;;GA;;;BA)"
	sd, err := windows.SecurityDescriptorFromString(sddl)
	if err != nil {
		s.logger.Error("Failed to create security descriptor", "error", err)
		sd = nil
	}

	var sa *windows.SecurityAttributes
	if sd != nil {
		sa = &windows.SecurityAttributes{
			Length:             uint32(unsafe.Sizeof(windows.SecurityAttributes{})),
			SecurityDescriptor: sd,
		}
	}

	for {
		pipePath, err := windows.UTF16PtrFromString(BridgePipeName)
		if err != nil {
			return fmt.Errorf("failed to convert pipe path: %w", err)
		}

		handle, err := windows.CreateNamedPipe(
			pipePath,
			windows.PIPE_ACCESS_DUPLEX,
			// Use MESSAGE mode like Weasel for more reliable message boundaries
			windows.PIPE_TYPE_MESSAGE|windows.PIPE_READMODE_MESSAGE|windows.PIPE_WAIT,
			windows.PIPE_UNLIMITED_INSTANCES,
			PipeBufferSize, // 64KB like Weasel
			PipeBufferSize,
			0,
			sa,
		)

		if err != nil {
			return fmt.Errorf("failed to create named pipe: %w", err)
		}

		s.logger.Debug("Waiting for C++ Bridge connection...")

		err = windows.ConnectNamedPipe(handle, nil)
		if err != nil && err != windows.ERROR_PIPE_CONNECTED {
			windows.CloseHandle(handle)
			continue
		}

		// Create pipe writer for this client
		writer := &pipeWriter{handle: handle}

		s.mu.Lock()
		s.clientCount++
		clientID := s.clientCount
		s.activeHandles[handle] = writer
		s.mu.Unlock()

		s.logger.Info("C++ Bridge connected", "clientID", clientID)

		// Handle client in a separate goroutine to allow concurrent connections
		go func(h windows.Handle, id int) {
			s.handleClient(h, id)

			s.mu.Lock()
			delete(s.activeHandles, h)
			activeCount := len(s.activeHandles)
			s.mu.Unlock()

			// Notify handler that a client disconnected
			s.handler.HandleClientDisconnected(activeCount)
		}(handle, clientID)
	}
}

func (s *Server) handleClient(handle windows.Handle, clientID int) {
	defer windows.CloseHandle(handle)

	s.logger.Debug("Handling client", "clientID", clientID)

	// Create a pipe reader wrapper
	reader := &pipeReader{handle: handle}
	writer := &pipeWriter{handle: handle}

	for {
		// Read header
		header, err := s.codec.ReadHeader(reader)
		if err != nil {
			if err != io.EOF {
				s.logger.Error("Failed to read header from Bridge", "clientID", clientID, "error", err)
			}
			break
		}

		// Read payload
		payload, err := s.codec.ReadPayload(reader, header.Length)
		if err != nil {
			s.logger.Error("Failed to read payload from Bridge", "clientID", clientID, "error", err)
			break
		}

		// Check if this is an async request (no response expected)
		isAsync := s.codec.IsAsyncRequest(header)

		// Handle batch events
		if header.Command == ipc.CmdBatchEvents {
			s.handleBatchEvents(header, payload, writer, clientID)
			continue
		}

		// Process request with timeout
		response := s.processRequestWithTimeout(header, payload, clientID)

		// Skip response for async requests
		if isAsync {
			s.logger.Debug("Async request processed, no response sent", "clientID", clientID, "command", fmt.Sprintf("0x%04X", header.Command))
			continue
		}

		// Write response
		if err := s.codec.WriteMessage(writer, response); err != nil {
			s.logger.Error("Failed to write response to Bridge", "clientID", clientID, "error", err)
			break
		}
	}

	s.logger.Info("C++ Bridge disconnected", "clientID", clientID)
}

// pipeReader wraps windows.Handle for io.Reader
// In MESSAGE mode, each ReadFile returns a complete message
type pipeReader struct {
	handle    windows.Handle
	msgBuffer []byte // Buffer for current message
	msgOffset int    // Current read offset in msgBuffer
}

func (r *pipeReader) Read(p []byte) (int, error) {
	// If we have buffered data from a previous message read, return that first
	if r.msgOffset < len(r.msgBuffer) {
		n := copy(p, r.msgBuffer[r.msgOffset:])
		r.msgOffset += n
		return n, nil
	}

	// Read a new message from the pipe
	// In MESSAGE mode, we need a buffer large enough for the entire message
	readBuf := make([]byte, PipeBufferSize)
	var bytesRead uint32

	err := windows.ReadFile(r.handle, readBuf, &bytesRead, nil)
	if err != nil {
		// Handle ERROR_MORE_DATA - message is larger than buffer
		if err == windows.ERROR_MORE_DATA {
			// This shouldn't happen with our 64KB buffer, but handle it anyway
			r.msgBuffer = make([]byte, bytesRead)
			copy(r.msgBuffer, readBuf[:bytesRead])
			r.msgOffset = 0

			// Read remaining data
			for {
				err = windows.ReadFile(r.handle, readBuf, &bytesRead, nil)
				if err == nil {
					r.msgBuffer = append(r.msgBuffer, readBuf[:bytesRead]...)
					break
				} else if err == windows.ERROR_MORE_DATA {
					r.msgBuffer = append(r.msgBuffer, readBuf[:bytesRead]...)
					continue
				} else {
					return 0, err
				}
			}

			n := copy(p, r.msgBuffer[r.msgOffset:])
			r.msgOffset += n
			return n, nil
		}
		return 0, err
	}

	if bytesRead == 0 {
		return 0, io.EOF
	}

	// Store the message in buffer for subsequent reads
	r.msgBuffer = readBuf[:bytesRead]
	r.msgOffset = 0

	n := copy(p, r.msgBuffer)
	r.msgOffset = n
	return n, nil
}

// pipeWriter wraps windows.Handle for io.Writer
type pipeWriter struct {
	handle windows.Handle
}

func (w *pipeWriter) Write(p []byte) (int, error) {
	var bytesWritten uint32
	err := windows.WriteFile(w.handle, p, &bytesWritten, nil)
	if err != nil {
		return 0, err
	}
	return int(bytesWritten), nil
}

// processRequestWithTimeout wraps processRequest with a timeout
func (s *Server) processRequestWithTimeout(header *ipc.IpcHeader, payload []byte, clientID int) []byte {
	ctx, cancel := context.WithTimeout(context.Background(), RequestProcessTimeout)
	defer cancel()

	// Channel to receive the response
	resultCh := make(chan []byte, 1)

	go func() {
		resultCh <- s.processRequest(header, payload, clientID)
	}()

	select {
	case response := <-resultCh:
		return response
	case <-ctx.Done():
		s.logger.Error("Request processing timed out", "clientID", clientID, "command", header.Command)
		return s.codec.EncodeAck()
	}
}

func (s *Server) processRequest(header *ipc.IpcHeader, payload []byte, clientID int) []byte {
	s.logger.Debug("Processing Bridge request", "clientID", clientID, "command", fmt.Sprintf("0x%04X", header.Command))

	switch header.Command {
	case ipc.CmdKeyEvent:
		return s.handleKeyEvent(payload, clientID)

	case ipc.CmdCommitRequest:
		return s.handleCommitRequest(payload, clientID)

	case ipc.CmdFocusGained:
		return s.handleFocusGained(payload, clientID)

	case ipc.CmdFocusLost:
		s.handler.HandleFocusLost()
		return s.codec.EncodeAck()

	case ipc.CmdIMEActivated:
		s.logger.Info("IME activated (user switched back to this IME)", "clientID", clientID)
		statusUpdate := s.handler.HandleIMEActivated()
		if statusUpdate != nil {
			return s.encodeStatusUpdate(statusUpdate)
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

	case ipc.CmdCaretUpdate:
		return s.handleCaretUpdate(payload, clientID)

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
			"text", result.Text, "modeChanged", result.ModeChanged, "newComposition", result.NewComposition)
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

func (s *Server) handleFocusGained(payload []byte, clientID int) []byte {
	// Parse optional caret data
	if len(payload) >= 12 {
		caretPayload, err := s.codec.DecodeCaretPayload(payload)
		if err == nil {
			s.logger.Debug("Focus gained with caret", "x", caretPayload.X, "y", caretPayload.Y)
			s.handler.HandleCaretUpdate(CaretData{
				X:      int(caretPayload.X),
				Y:      int(caretPayload.Y),
				Height: int(caretPayload.Height),
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
		X:      int(caretPayload.X),
		Y:      int(caretPayload.Y),
		Height: int(caretPayload.Height),
	})

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
func (s *Server) handleBatchEvents(header *ipc.IpcHeader, payload []byte, writer *pipeWriter, clientID int) {
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
		response := s.processRequestWithTimeout(event.Header, event.Payload, clientID)

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
	)
}

// startPushPipeListener starts the push pipe listener for state push
func (s *Server) startPushPipeListener() {
	s.logger.Info("Starting Push pipe listener", "pipe", PushPipeName)

	// Create security descriptor allowing Everyone, SYSTEM, and Administrators
	sddl := "D:P(A;;GA;;;WD)(A;;GA;;;SY)(A;;GA;;;BA)"
	sd, err := windows.SecurityDescriptorFromString(sddl)
	if err != nil {
		s.logger.Error("Failed to create security descriptor for push pipe", "error", err)
		sd = nil
	}

	var sa *windows.SecurityAttributes
	if sd != nil {
		sa = &windows.SecurityAttributes{
			Length:             uint32(unsafe.Sizeof(windows.SecurityAttributes{})),
			SecurityDescriptor: sd,
		}
	}

	for {
		pipePath, err := windows.UTF16PtrFromString(PushPipeName)
		if err != nil {
			s.logger.Error("Failed to convert push pipe path", "error", err)
			time.Sleep(1 * time.Second)
			continue
		}

		handle, err := windows.CreateNamedPipe(
			pipePath,
			windows.PIPE_ACCESS_OUTBOUND, // Write-only for push
			windows.PIPE_TYPE_MESSAGE|windows.PIPE_WAIT,
			windows.PIPE_UNLIMITED_INSTANCES,
			PipeBufferSize,
			0, // No input buffer needed
			0,
			sa,
		)

		if err != nil {
			s.logger.Error("Failed to create push pipe", "error", err)
			time.Sleep(1 * time.Second)
			continue
		}

		s.logger.Debug("Waiting for push pipe connection...")

		err = windows.ConnectNamedPipe(handle, nil)
		if err != nil && err != windows.ERROR_PIPE_CONNECTED {
			windows.CloseHandle(handle)
			continue
		}

		writer := &pipeWriter{handle: handle}

		s.pushMu.Lock()
		s.pushClientCount++
		clientID := s.pushClientCount
		s.pushClients[handle] = writer
		s.pushMu.Unlock()

		s.logger.Info("Push pipe client connected", "clientID", clientID)

		// Monitor client disconnection in a separate goroutine
		go func(h windows.Handle, id int) {
			// Wait for pipe to break (client disconnects)
			for {
				// Try to write a small test message (or wait for error on next push)
				time.Sleep(5 * time.Second)

				s.pushMu.RLock()
				_, exists := s.pushClients[h]
				s.pushMu.RUnlock()

				if !exists {
					break
				}
			}
		}(handle, clientID)
	}
}

// PushStateToAllClients broadcasts state update to all connected TSF clients
// This is used for proactive state push (e.g., when mode changes via toolbar click)
func (s *Server) PushStateToAllClients(status *StatusUpdateData) {
	if status == nil {
		return
	}

	// Encode the state push message using CMD_STATE_PUSH
	encoded := s.encodeStatePush(status)

	// Get all push clients
	s.pushMu.RLock()
	clients := make([]windows.Handle, 0, len(s.pushClients))
	writers := make([]*pipeWriter, 0, len(s.pushClients))
	for h, writer := range s.pushClients {
		clients = append(clients, h)
		writers = append(writers, writer)
	}
	clientCount := len(clients)
	s.pushMu.RUnlock()

	if clientCount == 0 {
		s.logger.Debug("No push pipe clients to send state to")
		return
	}

	s.logger.Info("Pushing state to TSF clients via push pipe",
		"count", clientCount,
		"chineseMode", status.ChineseMode,
		"fullWidth", status.FullWidth,
		"capsLock", status.CapsLock)

	// Send to all clients
	var failedHandles []windows.Handle
	successCount := 0
	for i, writer := range writers {
		if err := s.codec.WriteMessage(writer, encoded); err != nil {
			s.logger.Warn("Failed to push state to client", "error", err)
			failedHandles = append(failedHandles, clients[i])
		} else {
			successCount++
		}
	}

	// Remove failed clients
	if len(failedHandles) > 0 {
		s.pushMu.Lock()
		for _, h := range failedHandles {
			delete(s.pushClients, h)
			windows.CloseHandle(h)
		}
		s.pushMu.Unlock()
	}

	s.logger.Info("State push completed", "success", successCount, "total", clientCount)
}

// encodeStatePush encodes a state push message (CMD_STATE_PUSH)
func (s *Server) encodeStatePush(status *StatusUpdateData) []byte {
	return s.codec.EncodeStatePush(
		status.ChineseMode,
		status.FullWidth,
		status.ChinesePunctuation,
		status.ToolbarVisible,
		status.CapsLock,
	)
}

// GetActiveClientCount returns the number of active TSF clients
func (s *Server) GetActiveClientCount() int {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return len(s.activeHandles)
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
		return fmt.Sprintf("vk_%d", keyCode)
	}
}
