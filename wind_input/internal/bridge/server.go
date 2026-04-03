// Package bridge handles IPC communication with C++ TSF Bridge
package bridge

import (
	"fmt"
	"image"
	"io"
	"log/slog"
	"sync"
	"time"
	"unsafe"

	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/pkg/buildvariant"
	"golang.org/x/sys/windows"
)

var (
	kernel32                        = windows.NewLazySystemDLL("kernel32.dll")
	procGetNamedPipeClientProcessId = kernel32.NewProc("GetNamedPipeClientProcessId")
)

// getNamedPipeClientProcessId returns the process ID of the client connected to the named pipe
func getNamedPipeClientProcessId(handle windows.Handle) (uint32, error) {
	var processID uint32
	ret, _, err := procGetNamedPipeClientProcessId.Call(
		uintptr(handle),
		uintptr(unsafe.Pointer(&processID)),
	)
	if ret == 0 {
		return 0, err
	}
	return processID, nil
}

var (
	BridgePipeName = `\\.\pipe\wind_input` + buildvariant.Suffix()
	PushPipeName   = `\\.\pipe\wind_input` + buildvariant.Suffix() + `_push`
)

const (
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
	pushMu           sync.RWMutex
	pushClientCount  int
	pushClients      map[windows.Handle]*pipeWriter
	pushClientsByPID map[uint32]windows.Handle // Map process ID to push pipe handle
	pushHandleToPID  map[windows.Handle]uint32 // 反向映射：handle → PID，避免 O(n²) 查找

	// Active client tracking (for secure, targeted push)
	activeMu        sync.RWMutex
	activeProcessID uint32 // Process ID of the client that has focus

	// Host render manager (for Band window proxy rendering)
	hostRender *HostRenderManager
}

// NewServer creates a new Bridge IPC server
func NewServer(handler MessageHandler, logger *slog.Logger) *Server {
	return &Server{
		handler:          handler,
		logger:           logger,
		codec:            ipc.NewBinaryCodec(),
		activeHandles:    make(map[windows.Handle]*pipeWriter),
		pushClients:      make(map[windows.Handle]*pipeWriter),
		pushClientsByPID: make(map[uint32]windows.Handle),
		pushHandleToPID:  make(map[windows.Handle]uint32),
	}
}

// SetHostRenderManager sets the host render manager for Band window proxy rendering.
func (s *Server) SetHostRenderManager(hrm *HostRenderManager) {
	s.hostRender = hrm
}

// GetHostRenderManager returns the host render manager.
func (s *Server) GetHostRenderManager() *HostRenderManager {
	return s.hostRender
}

// GetActiveHostRender returns write/hide functions if the active process has host rendering.
// Returns nil functions if host rendering is not active.
func (s *Server) GetActiveHostRender() (writeFrame func(img *image.RGBA, x, y int) error, hideFunc func()) {
	if s.hostRender == nil {
		return nil, nil
	}

	s.activeMu.RLock()
	pid := s.activeProcessID
	s.activeMu.RUnlock()

	if pid == 0 {
		return nil, nil
	}

	state := s.hostRender.GetActiveState(pid)
	if state == nil || state.SHM == nil {
		return nil, nil
	}

	shm := state.SHM
	return shm.WriteFrame, shm.WriteHide
}

// Start begins listening for connections from C++ Bridge
func (s *Server) Start() error {
	s.logger.Info("Starting Bridge IPC server (binary protocol)", "pipe", BridgePipeName)

	// Start the push pipe listener in a separate goroutine
	go s.startPushPipeListener()

	// Allow desktop clients plus AppContainer/modern hosts (e.g. Start menu search).
	// S:(ML;;NW;;;LW) = Mandatory Label: Low integrity — required for UWP/AppContainer
	//   processes (Microsoft Store, Start Menu) which run at low integrity level.
	//   Without this, the mandatory integrity check blocks access before DACL evaluation.
	// D: = DACL: WD=Everyone, SY=SYSTEM, BA=Administrators, AC=ALL APPLICATION PACKAGES
	sddl := "D:P(A;;GA;;;WD)(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AC)S:(ML;;NW;;;LW)"
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
			pid := s.handleClient(h, id)

			// Capture the current setup sequence BEFORE acquiring the main lock.
			// This prevents a race where the old connection's cleanup goroutine
			// destroys a newer connection's SharedMemory for the same PID.
			var setupSeq uint64
			if s.hostRender != nil && pid != 0 {
				setupSeq = s.hostRender.GetSetupSeq(pid)
			}

			s.mu.Lock()
			delete(s.activeHandles, h)
			activeCount := len(s.activeHandles)
			s.mu.Unlock()

			// Clean up host render resources only if the generation matches
			if s.hostRender != nil && pid != 0 && setupSeq != 0 {
				s.hostRender.CleanupClient(pid, setupSeq)
			}

			// Notify handler that a client disconnected
			s.handler.HandleClientDisconnected(activeCount)
		}(handle, clientID)
	}
}

func (s *Server) handleClient(handle windows.Handle, clientID int) uint32 {
	defer windows.CloseHandle(handle)

	// Get the client's process ID for tracking active client
	processID, err := getNamedPipeClientProcessId(handle)
	if err != nil {
		s.logger.Warn("Failed to get client process ID", "clientID", clientID, "error", err)
		processID = 0 // Continue without process ID tracking
	} else {
		s.logger.Debug("Handling client", "clientID", clientID, "processID", processID)
	}

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
			s.handleBatchEvents(header, payload, writer, clientID, processID)
			continue
		}

		// Process request with timeout
		response := s.processRequestWithTimeout(header, payload, clientID, processID)

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
	return processID
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
