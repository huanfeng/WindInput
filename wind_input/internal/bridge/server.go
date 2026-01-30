// Package bridge handles IPC communication with C++ TSF Bridge
package bridge

import (
	"context"
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

	// Timeout for processing a single request
	RequestProcessTimeout = 200 * time.Millisecond
)

// Server handles IPC communication with C++ TSF Bridge
type Server struct {
	logger  *slog.Logger
	handler MessageHandler
	codec   *ipc.BinaryCodec

	mu            sync.Mutex
	clientCount   int
	activeHandles map[windows.Handle]bool
}

// NewServer creates a new Bridge IPC server
func NewServer(handler MessageHandler, logger *slog.Logger) *Server {
	return &Server{
		handler:       handler,
		logger:        logger,
		codec:         ipc.NewBinaryCodec(),
		activeHandles: make(map[windows.Handle]bool),
	}
}

// Start begins listening for connections from C++ Bridge
func (s *Server) Start() error {
	s.logger.Info("Starting Bridge IPC server (binary protocol)", "pipe", BridgePipeName)

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
			windows.PIPE_TYPE_BYTE|windows.PIPE_READMODE_BYTE|windows.PIPE_WAIT,
			windows.PIPE_UNLIMITED_INSTANCES,
			4096,
			4096,
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

		s.mu.Lock()
		s.clientCount++
		clientID := s.clientCount
		s.activeHandles[handle] = true
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

		// Process request with timeout
		response := s.processRequestWithTimeout(header, payload, clientID)

		// Write response
		if err := s.codec.WriteMessage(writer, response); err != nil {
			s.logger.Error("Failed to write response to Bridge", "clientID", clientID, "error", err)
			break
		}
	}

	s.logger.Info("C++ Bridge disconnected", "clientID", clientID)
}

// pipeReader wraps windows.Handle for io.Reader
type pipeReader struct {
	handle windows.Handle
}

func (r *pipeReader) Read(p []byte) (int, error) {
	var bytesRead uint32
	err := windows.ReadFile(r.handle, p, &bytesRead, nil)
	if err != nil {
		return 0, err
	}
	if bytesRead == 0 {
		return 0, io.EOF
	}
	return int(bytesRead), nil
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
	}

	s.logger.Debug("Key event", "clientID", clientID,
		"keyCode", keyData.KeyCode,
		"modifiers", fmt.Sprintf("0x%X", keyData.Modifiers),
		"event", eventType)

	result := s.handler.HandleKeyEvent(keyData)
	if result == nil {
		// Key not handled by IME, tell C++ to pass it through to the system
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
