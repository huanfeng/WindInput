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

// hostRenderKinds 列出当前接入 host render 的窗口种类。每类有独立的 SHM 段 + per-PID
// 唤醒 event + DLL band 窗口——因为候选/tooltip/状态窗可能同时可见，不能共用单 bitmap
// 通道。DLL 侧 _EnsureHostRenderSetup 通用遍历条目建窗，新增种类只需加入此列表。
var hostRenderKinds = []ipc.HostWindowKind{
	ipc.HostWindowCandidate,
	ipc.HostWindowTooltip,
	ipc.HostWindowStatus,
}

// winSHMNameFor 返回某窗口种类的全局 SHM 段名。候选保持原名（向后兼容），其余加后缀。
// 变体后缀隔离 release/debug，避免两变体服务互相打开对方的 section 导致渲染串扰。
// Windows 命名 file-mapping 的物理页在所有映射进程间共享，故每类物理内存恒为一份，
// 与同时注入的进程数无关。
func winSHMNameFor(kind ipc.HostWindowKind) string {
	base := "Local\\WindInput_SHM" + buildvariant.Suffix()
	switch kind {
	case ipc.HostWindowTooltip:
		return base + "_TIP"
	case ipc.HostWindowStatus:
		return base + "_STS"
	default:
		return base
	}
}

// winEvtNameFor 返回某 PID 某窗口种类的私有唤醒 event 名。按 (PID, kind) 隔离：Go 只
// signal 焦点进程对应窗口的 event，背景进程或其它窗口的渲染线程休眠，避免多个 reader 争
// 抢同一 auto-reset event 只唤醒其中一个（不确定是谁）导致拿不到帧的串扰。候选保持原名
// 向后兼容。
func winEvtNameFor(pid uint32, kind ipc.HostWindowKind) string {
	base := fmt.Sprintf("Local\\WindInput_EVT_%d", pid)
	switch kind {
	case ipc.HostWindowTooltip:
		return base + "_TIP"
	case ipc.HostWindowStatus:
		return base + "_STS"
	default:
		return base
	}
}

// hostRenderChannel 是单个窗口种类的发送通道：全局共享段 + 本进程私有唤醒 event。
type hostRenderChannel struct {
	SHM   *SharedMemory // 该 kind 的全局共享段（懒建常驻，所有 state 共享同一个）
	Event *NamedEvent   // 本进程该 kind 私有唤醒 event
}

// HostRenderState tracks host rendering state for a single client process, keyed by
// window kind. Each channel's SHM points at the global per-kind section; its Event is
// this process's private wake event for that kind.
type HostRenderState struct {
	ProcessID uint32
	channels  map[ipc.HostWindowKind]*hostRenderChannel
	Active    bool   // Whether host render is currently active
	SetupSeq  uint64 // Monotonic counter to distinguish old vs new state
}

// WriteFrame writes a frame to the given kind's SHM, then wakes ONLY this process's
// render thread for that kind. server.GetActiveHostRenderFor hands this back bound to
// the active PID + kind, so frames wake only the right band window of the right process.
func (st *HostRenderState) WriteFrame(kind ipc.HostWindowKind, img *image.RGBA, x, y int, rects []ipc.CandidateHitRect, renderedHover int) error {
	ch := st.channels[kind]
	if ch == nil || ch.SHM == nil {
		return fmt.Errorf("host render channel %d unavailable", kind)
	}
	if err := ch.SHM.WriteFrame(img, x, y, rects, renderedHover); err != nil {
		return err
	}
	ch.Event.Signal()
	return nil
}

// WriteHide writes a hide frame to the given kind's SHM, then wakes only this process.
func (st *HostRenderState) WriteHide(kind ipc.HostWindowKind) {
	ch := st.channels[kind]
	if ch == nil || ch.SHM == nil {
		return
	}
	ch.SHM.WriteHide()
	ch.Event.Signal()
}

// HostRenderManager manages host rendering for whitelisted processes.
//
// 每窗口种类一块全局 SHM + per-(PID,kind) event 模型：每类窗口（候选/tooltip/状态）
// 各有一份全局共享 SHM（内存恒一份），但每个宿主进程对每类有独立唤醒 event。这样多个
// 高 Band 进程（如多个 SearchHost 实例）可同时安全工作，且同一进程内多类窗口互不串扰
// ——Go 只 signal 焦点进程对应窗口的 event。
type HostRenderManager struct {
	mu       sync.Mutex
	logger   *slog.Logger
	patterns []string                             // 小写进程名模式，支持 filepath.Match 通配符（"*" 短路匹配全部）
	shms     map[ipc.HostWindowKind]*SharedMemory // 每窗口种类的全局共享段（懒建常驻）
	clients  map[uint32]*HostRenderState          // PID -> state（持 per-(PID,kind) event）
	setupSeq uint64                               // Monotonic counter for setup generation
}

// NewHostRenderManager creates a new host render manager with the given whitelist.
func NewHostRenderManager(logger *slog.Logger, processNames []string) *HostRenderManager {
	return &HostRenderManager{
		logger:   logger,
		patterns: normalizePatterns(processNames),
		shms:     make(map[ipc.HostWindowKind]*SharedMemory),
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

// SetupHostRender lazily creates one global shared-memory section per host window kind
// and one per-PID wake event per kind for the client, returning one setup entry per kind
// for the DLL (which creates one band window per entry).
func (m *HostRenderManager) SetupHostRender(processID uint32) ([]ipc.HostRenderSetupEntry, error) {
	m.mu.Lock()
	defer m.mu.Unlock()

	// 重建该 PID 的私有 events（若已存在先关旧的所有 kind）
	if old, ok := m.clients[processID]; ok {
		for _, ch := range old.channels {
			if ch.Event != nil {
				ch.Event.Close()
			}
		}
		delete(m.clients, processID)
	}

	state := &HostRenderState{
		ProcessID: processID,
		channels:  make(map[ipc.HostWindowKind]*hostRenderChannel, len(hostRenderKinds)),
		Active:    true,
	}
	entries := make([]ipc.HostRenderSetupEntry, 0, len(hostRenderKinds))

	for _, kind := range hostRenderKinds {
		// 懒建该 kind 的全局共享段（一次，常驻）
		shm := m.shms[kind]
		if shm == nil {
			s, err := NewSharedMemory(winSHMNameFor(kind), ipc.MaxSharedRenderSize)
			if err != nil {
				return nil, fmt.Errorf("create host render SHM kind %d: %w", kind, err)
			}
			m.shms[kind] = s
			shm = s
			m.logger.Info("Host render SHM created", "kind", kind, "shmName", s.Name())
		}

		evtName := winEvtNameFor(processID, kind)
		evt, err := newNamedEvent(evtName)
		if err != nil {
			return nil, fmt.Errorf("create wake event kind %d for PID %d: %w", kind, processID, err)
		}
		state.channels[kind] = &hostRenderChannel{SHM: shm, Event: evt}
		entries = append(entries, ipc.HostRenderSetupEntry{
			WindowKind:    kind,
			MaxBufferSize: shm.Size(),
			ShmName:       shm.Name(),
			EventName:     evtName,
		})
	}

	m.setupSeq++
	state.SetupSeq = m.setupSeq
	m.clients[processID] = state

	m.logger.Info("Host render setup created", "processID", processID, "kinds", len(entries))
	return entries, nil
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

// CleanupClient removes host render state for a disconnected client. Only this PID's
// per-kind wake events are closed; the global per-kind SHM sections persist (other
// processes share them). The expectedSeq guard prevents an old connection's cleanup
// goroutine from closing a newer connection's events for the same (recycled) PID.
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

	for _, ch := range state.channels {
		if ch.Event != nil {
			ch.Event.Close()
		}
	}
	delete(m.clients, processID)
	m.logger.Info("Host render cleanup", "processID", processID, "seq", expectedSeq)
}

// CleanupAll closes all per-PID events and all per-kind global shared memory sections.
// Called on service shutdown.
func (m *HostRenderManager) CleanupAll() {
	m.mu.Lock()
	defer m.mu.Unlock()

	for pid, state := range m.clients {
		for _, ch := range state.channels {
			if ch.Event != nil {
				ch.Event.Close()
			}
		}
		delete(m.clients, pid)
	}
	for kind, shm := range m.shms {
		if shm != nil {
			shm.Close()
		}
		delete(m.shms, kind)
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
