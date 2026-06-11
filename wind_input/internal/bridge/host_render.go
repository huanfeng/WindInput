//go:build windows

package bridge

import (
	"fmt"
	"image"
	"log/slog"
	"path/filepath"
	"strings"
	"sync"
	"unsafe"

	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/pkg/buildvariant"
	"golang.org/x/sys/windows"
)

var (
	procOpenProcess                = modkernel32.NewProc("OpenProcess")
	procQueryFullProcessImageNameW = modkernel32.NewProc("QueryFullProcessImageNameW")
)

const processQueryLimitedInformation = 0x1000

// 单块全局 SHM 命名（变体后缀隔离 release/debug，避免两变体服务互相打开对方的
// section 导致渲染串扰）。所有白名单宿主进程的 DLL 打开同一段命名 section —
// Windows 命名 file-mapping 的物理页在所有映射进程间共享，因此物理内存恒为一份，
// 与同时注入的进程数无关；服务进程也只 MapViewOfFile 一次。
//
// 唤醒 event 则按 PID 隔离（`Local\WindInput_EVT_<PID>`）：Go 只 signal 焦点进程的
// event，背景进程的渲染线程因此休眠，避免多个 reader 争抢同一 event 时 auto-reset
// 只唤醒其中一个（不确定是谁）导致焦点进程拿不到帧、候选不显示的串扰回归。
var winSHMName = "Local\\WindInput_SHM" + buildvariant.Suffix()

// HostRenderState tracks host rendering state for a single client process.
// SHM 指向全局共享段（所有 state 共享同一个）；Event 是本进程私有的唤醒 event。
type HostRenderState struct {
	ProcessID uint32
	SHM       *SharedMemory // 全局共享段（懒建常驻，所有 state 共享）
	Event     *NamedEvent   // 本进程私有唤醒 event
	Active    bool          // Whether host render is currently active
	SetupSeq  uint64        // Monotonic counter to distinguish old vs new state
}

// WriteFrame writes the frame to the shared global SHM, then wakes ONLY this
// process's render thread. server.GetActiveHostRender hands this back bound to the
// active PID, so frames written while a process holds focus wake only that process.
func (st *HostRenderState) WriteFrame(img *image.RGBA, x, y int, rects []ipc.CandidateHitRect, renderedHover int) error {
	if err := st.SHM.WriteFrame(img, x, y, rects, renderedHover); err != nil {
		return err
	}
	st.Event.Signal()
	return nil
}

// WriteHide writes a hide frame to the shared SHM, then wakes only this process.
func (st *HostRenderState) WriteHide() {
	st.SHM.WriteHide()
	st.Event.Signal()
}

// HostRenderManager manages host rendering for whitelisted processes.
//
// 单块全局 SHM + per-PID event 模型：全局一份共享 SHM（内存恒一份），但每个宿主
// 进程有独立唤醒 event。这样多个高 Band 进程（如多个 SearchHost 实例）可同时安全
// 工作——Go 只 signal 焦点进程的 event，无串扰。
type HostRenderManager struct {
	mu       sync.Mutex
	logger   *slog.Logger
	patterns []string                    // 小写进程名模式，支持 filepath.Match 通配符（"*" 短路匹配全部）
	shm      *SharedMemory               // 全局共享段（懒建常驻）
	clients  map[uint32]*HostRenderState // PID -> state（持 per-PID event）
	setupSeq uint64                      // Monotonic counter for setup generation
}

// NewHostRenderManager creates a new host render manager with the given whitelist.
func NewHostRenderManager(logger *slog.Logger, processNames []string) *HostRenderManager {
	return &HostRenderManager{
		logger:   logger,
		patterns: normalizePatterns(processNames),
		clients:  make(map[uint32]*HostRenderState),
	}
}

// normalizePatterns 小写化白名单模式，保持顺序。
func normalizePatterns(processNames []string) []string {
	patterns := make([]string, 0, len(processNames))
	for _, name := range processNames {
		name = strings.TrimSpace(name)
		if name == "" {
			continue
		}
		patterns = append(patterns, strings.ToLower(name))
	}
	return patterns
}

// UpdateWhitelist updates the process whitelist (e.g. after config reload).
func (m *HostRenderManager) UpdateWhitelist(processNames []string) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.patterns = normalizePatterns(processNames)
}

// matchLocked 在已持有 m.mu 的前提下，按通配符模式匹配进程名（已小写）。
// 支持 filepath.Match 语法（* ? [..]）；模式 "*" 单独短路为"匹配全部进程"（全局模式）。
func (m *HostRenderManager) matchLocked(lowerName string) bool {
	for _, p := range m.patterns {
		if p == "*" {
			return true
		}
		if ok, err := filepath.Match(p, lowerName); err == nil && ok {
			return true
		}
	}
	return false
}

// IsProcessWhitelisted checks if a process should use host rendering.
func (m *HostRenderManager) IsProcessWhitelisted(processID uint32) bool {
	if processID == 0 {
		return false
	}

	name := GetProcessName(processID) // syscall 放在锁外
	if name == "" {
		return false
	}

	m.mu.Lock()
	defer m.mu.Unlock()
	return m.matchLocked(strings.ToLower(name))
}

// SetupHostRender lazily creates the single global shared memory and a per-PID wake
// event for the client, returning the setup payload for the DLL.
func (m *HostRenderManager) SetupHostRender(processID uint32) (*ipc.HostRenderSetupPayload, error) {
	m.mu.Lock()
	defer m.mu.Unlock()

	// 懒建全局共享段（一次，常驻）
	if m.shm == nil {
		shm, err := NewSharedMemory(winSHMName, ipc.MaxSharedRenderSize)
		if err != nil {
			return nil, err
		}
		m.shm = shm
		m.logger.Info("Host render global SHM created",
			"shmName", winSHMName,
			"maxSize", ipc.MaxSharedRenderSize)
	}

	// 重建该 PID 的私有 event（若已存在先关旧的）
	if old, ok := m.clients[processID]; ok {
		if old.Event != nil {
			old.Event.Close()
		}
		delete(m.clients, processID)
	}

	evtName := fmt.Sprintf("Local\\WindInput_EVT_%d", processID)
	evt, err := newNamedEvent(evtName)
	if err != nil {
		return nil, fmt.Errorf("failed to create wake event for PID %d: %w", processID, err)
	}

	m.setupSeq++
	m.clients[processID] = &HostRenderState{
		ProcessID: processID,
		SHM:       m.shm,
		Event:     evt,
		Active:    true,
		SetupSeq:  m.setupSeq,
	}

	m.logger.Info("Host render setup created",
		"processID", processID,
		"shmName", winSHMName,
		"evtName", evtName)

	return &ipc.HostRenderSetupPayload{
		MaxBufferSize: m.shm.Size(),
		ShmName:       m.shm.Name(),
		EventName:     evtName,
	}, nil
}

// GetSetupSeq returns the current setup sequence for a process, or 0 if not found.
// Used by disconnect handlers to pass to CleanupClient for race-safe cleanup.
func (m *HostRenderManager) GetSetupSeq(processID uint32) uint64 {
	m.mu.Lock()
	defer m.mu.Unlock()
	if state, ok := m.clients[processID]; ok {
		return state.SetupSeq
	}
	return 0
}

// GetActiveState returns the host render state for a process, or nil if not active.
// Presence in the clients map implies the process was whitelisted at setup time.
func (m *HostRenderManager) GetActiveState(processID uint32) *HostRenderState {
	m.mu.Lock()
	defer m.mu.Unlock()
	state := m.clients[processID]
	if state != nil && state.Active {
		return state
	}
	return nil
}

// CleanupClient removes host render state for a disconnected client. Only the
// per-PID wake event is closed; the global SHM persists (other processes share it).
// The expectedSeq guard prevents an old connection's cleanup goroutine from closing
// a newer connection's event for the same (recycled) PID.
func (m *HostRenderManager) CleanupClient(processID uint32, expectedSeq uint64) {
	if processID == 0 {
		return
	}

	m.mu.Lock()
	defer m.mu.Unlock()

	state, ok := m.clients[processID]
	if !ok {
		return
	}

	if expectedSeq != 0 && state.SetupSeq != expectedSeq {
		m.logger.Info("Host render cleanup skipped: stale generation",
			"processID", processID, "expected", expectedSeq, "current", state.SetupSeq)
		return
	}

	if state.Event != nil {
		state.Event.Close()
	}
	delete(m.clients, processID)
	m.logger.Info("Host render cleanup", "processID", processID, "seq", expectedSeq)
}

// CleanupAll closes all per-PID events and the global shared memory. Called on
// service shutdown.
func (m *HostRenderManager) CleanupAll() {
	m.mu.Lock()
	defer m.mu.Unlock()

	for pid, state := range m.clients {
		if state.Event != nil {
			state.Event.Close()
		}
		delete(m.clients, pid)
	}
	if m.shm != nil {
		m.shm.Close()
		m.shm = nil
	}
}

// GetProcessName returns the executable name (e.g. "SearchHost.exe") for a process ID.
func GetProcessName(pid uint32) string {
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
