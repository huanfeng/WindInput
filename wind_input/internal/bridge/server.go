// Package bridge handles IPC communication with C++ TSF Bridge
package bridge

import (
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"sync"
	"unsafe"

	"golang.org/x/sys/windows"
)

const (
	BridgePipeName = `\\.\pipe\wind_input`
)

// KeyEventResult represents the result of handling a key event
type KeyEventResult struct {
	Type        ResponseType
	Text        string // For InsertText
	CaretPos    int    // For UpdateComposition
	ChineseMode bool   // For ModeChanged
}

// MessageHandler handles messages from C++ Bridge
type MessageHandler interface {
	HandleKeyEvent(data KeyEventData) *KeyEventResult
	HandleCaretUpdate(data CaretData) error
	HandleFocusLost()        // Called when focus is lost
	HandleToggleMode() bool  // Called when mode toggle requested, returns new chineseMode state
}

// Server handles IPC communication with C++ TSF Bridge
type Server struct {
	logger  *slog.Logger
	handler MessageHandler

	mu            sync.Mutex
	clientCount   int
	activeHandles map[windows.Handle]bool
}

// NewServer creates a new Bridge IPC server
func NewServer(handler MessageHandler, logger *slog.Logger) *Server {
	return &Server{
		handler:       handler,
		logger:        logger,
		activeHandles: make(map[windows.Handle]bool),
	}
}

// Start begins listening for connections from C++ Bridge
func (s *Server) Start() error {
	s.logger.Info("Starting Bridge IPC server", "pipe", BridgePipeName)

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

		s.logger.Info("Waiting for C++ Bridge connection...")

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
			s.mu.Unlock()
		}(handle, clientID)
	}
}

func (s *Server) handleClient(handle windows.Handle, clientID int) {
	defer windows.CloseHandle(handle)

	s.logger.Info("Handling client", "clientID", clientID)

	for {
		request, err := s.readMessage(handle, clientID)
		if err != nil {
			if err != io.EOF {
				s.logger.Error("Failed to read message from Bridge", "clientID", clientID, "error", err)
			}
			break
		}

		response := s.processRequest(request, clientID)

		if err := s.writeMessage(handle, response, clientID); err != nil {
			s.logger.Error("Failed to write response to Bridge", "clientID", clientID, "error", err)
			break
		}
	}

	s.logger.Info("C++ Bridge disconnected", "clientID", clientID)
}

func (s *Server) readMessage(handle windows.Handle, clientID int) (*Request, error) {
	// Read message length (4 bytes) - must read exactly 4 bytes
	lengthBuf := make([]byte, 4)
	totalRead := uint32(0)
	for totalRead < 4 {
		var bytesRead uint32
		err := windows.ReadFile(handle, lengthBuf[totalRead:], &bytesRead, nil)
		if err != nil {
			return nil, err
		}
		if bytesRead == 0 {
			return nil, io.EOF
		}
		totalRead += bytesRead
	}

	length := *(*uint32)(unsafe.Pointer(&lengthBuf[0]))
	s.logger.Debug("Read message length", "clientID", clientID, "length", length)

	// Sanity check on length
	if length == 0 || length > 1024*1024 {
		return nil, fmt.Errorf("invalid message length: %d", length)
	}

	// Read message content - loop until all bytes are read
	buffer := make([]byte, length)
	totalRead = 0
	for totalRead < length {
		var bytesRead uint32
		err := windows.ReadFile(handle, buffer[totalRead:], &bytesRead, nil)
		if err != nil {
			return nil, err
		}
		if bytesRead == 0 {
			return nil, fmt.Errorf("incomplete read: expected %d, got %d", length, totalRead)
		}
		totalRead += bytesRead
	}

	s.logger.Info("Received from Bridge", "clientID", clientID, "json", string(buffer))

	// Parse JSON
	var request Request
	if err := json.Unmarshal(buffer, &request); err != nil {
		return nil, fmt.Errorf("failed to parse JSON: %w", err)
	}

	return &request, nil
}

func (s *Server) writeMessage(handle windows.Handle, response *Response, clientID int) error {
	// Serialize response
	data, err := json.Marshal(response)
	if err != nil {
		return fmt.Errorf("failed to serialize response: %w", err)
	}

	s.logger.Info("Sending to Bridge", "clientID", clientID, "json", string(data))

	// Write length prefix
	length := uint32(len(data))
	lengthBuf := (*[4]byte)(unsafe.Pointer(&length))[:]

	totalWritten := uint32(0)
	for totalWritten < 4 {
		var bytesWritten uint32
		err = windows.WriteFile(handle, lengthBuf[totalWritten:], &bytesWritten, nil)
		if err != nil {
			return err
		}
		totalWritten += bytesWritten
	}

	// Write content - loop until all bytes are written
	totalWritten = 0
	for totalWritten < length {
		var bytesWritten uint32
		err = windows.WriteFile(handle, data[totalWritten:], &bytesWritten, nil)
		if err != nil {
			return err
		}
		totalWritten += bytesWritten
	}

	return nil
}

func (s *Server) processRequest(request *Request, clientID int) *Response {
	s.logger.Info("Processing Bridge request", "clientID", clientID, "type", request.Type)

	switch request.Type {
	case RequestTypeKeyEvent:
		// Parse key event data
		var keyData KeyEventData
		if err := json.Unmarshal(request.Data, &keyData); err != nil {
			s.logger.Error("Failed to parse key event data", "clientID", clientID, "error", err)
			return &Response{Type: ResponseTypeAck, Error: "invalid key event data"}
		}

		result := s.handler.HandleKeyEvent(keyData)
		if result == nil {
			return &Response{Type: ResponseTypeAck}
		}

		// Build response based on result
		switch result.Type {
		case ResponseTypeInsertText:
			s.logger.Info("Returning InsertText response", "clientID", clientID, "text", result.Text)
			return &Response{
				Type: ResponseTypeInsertText,
				Data: InsertTextData{Text: result.Text},
			}
		case ResponseTypeUpdateComposition:
			return &Response{
				Type: ResponseTypeUpdateComposition,
				Data: CompositionData{Text: result.Text, CaretPos: result.CaretPos},
			}
		case ResponseTypeClearComposition:
			return &Response{Type: ResponseTypeClearComposition}
		case ResponseTypeModeChanged:
			s.logger.Info("Returning ModeChanged response", "clientID", clientID, "chineseMode", result.ChineseMode)
			return &Response{
				Type: ResponseTypeModeChanged,
				Data: ModeChangedData{ChineseMode: result.ChineseMode},
			}
		default:
			return &Response{Type: ResponseTypeAck}
		}

	case RequestTypeFocusLost:
		s.handler.HandleFocusLost()
		return &Response{Type: ResponseTypeAck}

	case RequestTypeCaretUpdate:
		// Parse caret data
		var caretData CaretData
		if err := json.Unmarshal(request.Data, &caretData); err != nil {
			s.logger.Error("Failed to parse caret data", "clientID", clientID, "error", err)
			return &Response{Type: ResponseTypeAck, Error: "invalid caret data"}
		}

		s.logger.Info("Received caret update", "clientID", clientID, "x", caretData.X, "y", caretData.Y, "height", caretData.Height)

		// Call handler to update caret position
		if err := s.handler.HandleCaretUpdate(caretData); err != nil {
			s.logger.Error("Failed to handle caret update", "clientID", clientID, "error", err)
		}
		return &Response{Type: ResponseTypeAck}

	case RequestTypeToggleMode:
		// Toggle input mode and return new state
		chineseMode := s.handler.HandleToggleMode()
		s.logger.Info("Mode toggled", "clientID", clientID, "chineseMode", chineseMode)
		return &Response{
			Type: ResponseTypeModeChanged,
			Data: ModeChangedData{ChineseMode: chineseMode},
		}

	default:
		s.logger.Error("Unknown request type from Bridge", "clientID", clientID, "type", request.Type)
		return &Response{Type: ResponseTypeAck, Error: fmt.Sprintf("unknown request type: %s", request.Type)}
	}
}
