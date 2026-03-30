package bridge

import (
	"fmt"
	"log/slog"
	"strings"
	"sync"
	"unsafe"

	"github.com/huanfeng/wind_input/internal/ipc"
	"golang.org/x/sys/windows"
)

var (
	procOpenProcess                = modkernel32.NewProc("OpenProcess")
	procQueryFullProcessImageNameW = modkernel32.NewProc("QueryFullProcessImageNameW")
)

const processQueryLimitedInformation = 0x1000

// HostRenderState tracks host rendering state for a single client process.
type HostRenderState struct {
	ProcessID uint32
	SHM       *SharedMemory
	Active    bool // Whether host render is currently active
}

// HostRenderManager manages host rendering for whitelisted processes.
type HostRenderManager struct {
	mu        sync.RWMutex
	logger    *slog.Logger
	whitelist map[string]bool             // Lowercase process names
	clients   map[uint32]*HostRenderState // PID -> state
}

// NewHostRenderManager creates a new host render manager with the given whitelist.
func NewHostRenderManager(logger *slog.Logger, processNames []string) *HostRenderManager {
	wl := make(map[string]bool, len(processNames))
	for _, name := range processNames {
		wl[strings.ToLower(name)] = true
	}
	return &HostRenderManager{
		logger:    logger,
		whitelist: wl,
		clients:   make(map[uint32]*HostRenderState),
	}
}

// UpdateWhitelist updates the process whitelist (e.g. after config reload).
func (m *HostRenderManager) UpdateWhitelist(processNames []string) {
	m.mu.Lock()
	defer m.mu.Unlock()
	wl := make(map[string]bool, len(processNames))
	for _, name := range processNames {
		wl[strings.ToLower(name)] = true
	}
	m.whitelist = wl
}

// IsProcessWhitelisted checks if a process should use host rendering.
func (m *HostRenderManager) IsProcessWhitelisted(processID uint32) bool {
	if processID == 0 {
		return false
	}

	name := getProcessName(processID)
	if name == "" {
		return false
	}

	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.whitelist[strings.ToLower(name)]
}

// SetupHostRender creates shared memory for a whitelisted client.
// Returns the setup payload to send to the DLL, or nil if not applicable.
func (m *HostRenderManager) SetupHostRender(processID uint32) (*ipc.HostRenderSetupPayload, error) {
	m.mu.Lock()
	defer m.mu.Unlock()

	// Clean up existing state for this PID if any
	if old, ok := m.clients[processID]; ok {
		if old.SHM != nil {
			old.SHM.Close()
		}
		delete(m.clients, processID)
	}

	shmName := fmt.Sprintf("Local\\WindInput_SHM_%d", processID)
	evtName := fmt.Sprintf("Local\\WindInput_EVT_%d", processID)

	shm, err := NewSharedMemory(shmName, evtName, ipc.MaxSharedRenderSize)
	if err != nil {
		return nil, fmt.Errorf("failed to create shared memory for PID %d: %w", processID, err)
	}

	m.clients[processID] = &HostRenderState{
		ProcessID: processID,
		SHM:       shm,
		Active:    true,
	}

	m.logger.Info("Host render setup created",
		"processID", processID,
		"shmName", shmName,
		"evtName", evtName,
		"maxSize", ipc.MaxSharedRenderSize)

	return &ipc.HostRenderSetupPayload{
		MaxBufferSize: ipc.MaxSharedRenderSize,
		ShmName:       shmName,
		EventName:     evtName,
	}, nil
}

// GetActiveState returns the host render state for a process, or nil if not active.
func (m *HostRenderManager) GetActiveState(processID uint32) *HostRenderState {
	m.mu.RLock()
	defer m.mu.RUnlock()
	state := m.clients[processID]
	if state != nil && state.Active {
		return state
	}
	return nil
}

// CleanupClient removes host render state for a disconnected client.
func (m *HostRenderManager) CleanupClient(processID uint32) {
	if processID == 0 {
		return
	}

	m.mu.Lock()
	defer m.mu.Unlock()

	if state, ok := m.clients[processID]; ok {
		if state.SHM != nil {
			state.SHM.WriteHide()
			state.SHM.Close()
		}
		delete(m.clients, processID)
		m.logger.Info("Host render cleanup", "processID", processID)
	}
}

// CleanupAll closes all shared memory resources.
func (m *HostRenderManager) CleanupAll() {
	m.mu.Lock()
	defer m.mu.Unlock()

	for pid, state := range m.clients {
		if state.SHM != nil {
			state.SHM.Close()
		}
		delete(m.clients, pid)
	}
}

// getProcessName returns the executable name (e.g. "SearchHost.exe") for a process ID.
func getProcessName(pid uint32) string {
	hProcess, _, _ := procOpenProcess.Call(
		processQueryLimitedInformation,
		0,
		uintptr(pid),
	)
	if hProcess == 0 {
		return ""
	}
	defer windows.CloseHandle(windows.Handle(hProcess))

	var buf [windows.MAX_PATH]uint16
	size := uint32(windows.MAX_PATH)
	ret, _, _ := procQueryFullProcessImageNameW.Call(
		hProcess,
		0,
		uintptr(unsafe.Pointer(&buf[0])),
		uintptr(unsafe.Pointer(&size)),
	)
	if ret == 0 {
		return ""
	}

	fullPath := windows.UTF16ToString(buf[:size])
	// Extract just the filename
	for i := len(fullPath) - 1; i >= 0; i-- {
		if fullPath[i] == '\\' || fullPath[i] == '/' {
			return fullPath[i+1:]
		}
	}
	return fullPath
}
