//go:build windows

package bridge

import (
	"encoding/binary"
	"fmt"
	"image"
	"sync"
	"unsafe"

	"github.com/huanfeng/wind_input/internal/ipc"
	"golang.org/x/sys/windows"
)

var (
	modkernel32            = windows.NewLazySystemDLL("kernel32.dll")
	procCreateFileMappingW = modkernel32.NewProc("CreateFileMappingW")
	procMapViewOfFile      = modkernel32.NewProc("MapViewOfFile")
	procUnmapViewOfFile    = modkernel32.NewProc("UnmapViewOfFile")
	procCreateEventW       = modkernel32.NewProc("CreateEventW")
	procSetEvent           = modkernel32.NewProc("SetEvent")
)

const (
	fileMapAllAccess = 0xF001F
	pageReadWrite    = 0x04
)

// hostRenderSecurityAttributes builds SECURITY_ATTRIBUTES with an SDDL that grants
// AppContainer/UWP processes (SearchHost.exe, Start Menu) access. Shared by the
// shared-memory section and the per-PID named events so both are openable by the
// same low-integrity host processes. Returns nil sa if the descriptor fails (the
// kernel object is then created with the default ACL — still works for
// non-AppContainer hosts).
//
// SDDL: GA = Generic All; S:(ML;;NW;;;LW) = Low mandatory label, required for
// UWP/AppContainer processes. Verified to work with SearchHost.exe / Start Menu.
func hostRenderSecurityAttributes() *windows.SecurityAttributes {
	sddl := "D:P(A;;GA;;;WD)(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AC)S:(ML;;NW;;;LW)"
	sd, _ := windows.SecurityDescriptorFromString(sddl)
	if sd == nil {
		return nil
	}
	return &windows.SecurityAttributes{
		Length:             uint32(unsafe.Sizeof(windows.SecurityAttributes{})),
		SecurityDescriptor: sd,
		InheritHandle:      0,
	}
}

// SharedMemory manages a named shared memory region for host render bitmap transfer.
//
// Wake signaling is DECOUPLED from the region: the named section is global/shared
// across all host processes (one physical backing — Windows shares the pages of a
// named file-mapping among every process that maps it), while each host process is
// woken via its own NamedEvent (see host_render.go). WriteFrame/WriteHide therefore
// do NOT signal — the caller signals only the active process's event, which keeps a
// backgrounded process's render thread asleep and prevents cross-talk over the one
// shared section.
type SharedMemory struct {
	mu       sync.Mutex
	name     string
	size     uint32
	hMapping windows.Handle
	pView    unsafe.Pointer
	sequence uint32
}

// NewSharedMemory creates a named shared memory region.
// name: e.g. "Local\\WindInput_SHM"
// size: total size including header (e.g. MaxSharedRenderSize)
func NewSharedMemory(name string, size uint32) (*SharedMemory, error) {
	namePtr, err := windows.UTF16PtrFromString(name)
	if err != nil {
		return nil, fmt.Errorf("invalid shared memory name: %w", err)
	}

	sa := hostRenderSecurityAttributes()

	// Create file mapping with AppContainer-accessible security
	hMapping, _, err := procCreateFileMappingW.Call(
		uintptr(windows.InvalidHandle), // page file backed
		uintptr(unsafe.Pointer(sa)),
		pageReadWrite,
		0,             // high dword of size
		uintptr(size), // low dword of size
		uintptr(unsafe.Pointer(namePtr)),
	)
	if hMapping == 0 {
		return nil, fmt.Errorf("CreateFileMapping failed: %w", err)
	}

	// Map view
	pViewAddr, _, err := procMapViewOfFile.Call(
		hMapping,
		fileMapAllAccess,
		0, 0, // offset
		uintptr(size),
	)
	if pViewAddr == 0 {
		windows.CloseHandle(windows.Handle(hMapping))
		return nil, fmt.Errorf("MapViewOfFile failed: %w", err)
	}
	// Convert syscall uintptr return to unsafe.Pointer using the approved double-indirect pattern.
	pView := *(*unsafe.Pointer)(unsafe.Pointer(&pViewAddr))

	// Zero the header
	headerSlice := unsafe.Slice((*byte)(pView), ipc.SharedRenderHeaderSize)
	for i := range headerSlice {
		headerSlice[i] = 0
	}

	return &SharedMemory{
		name:     name,
		size:     size,
		hMapping: windows.Handle(hMapping),
		pView:    pView,
	}, nil
}

// WriteFrame writes a rendered candidate image to shared memory.
// img must be *image.RGBA. Performs RGBA→BGRA conversion inline.
// rects is the panel-local hit-test geometry for this frame (candidates + page
// buttons as Index -1/-2); it is embedded right after the pixel data so the DLL's
// host window can route mouse clicks/hover back to Go. Pass nil for a non-interactive
// frame. renderedHover is the candidate index actually highlighted in this frame
// (hover encoding: >=0 candidate, -1 none, -2 page-up, -3 page-down); the DLL syncs its
// hover-dedup baseline to it so re-hovering the same index after a content change still
// re-highlights. Does NOT signal any event — the caller wakes the active process's render
// thread via that process's NamedEvent.
func (sm *SharedMemory) WriteFrame(img *image.RGBA, screenX, screenY int, rects []ipc.CandidateHitRect, renderedHover int) error {
	sm.mu.Lock()
	defer sm.mu.Unlock()

	if sm.pView == nil {
		return fmt.Errorf("shared memory not mapped")
	}

	bounds := img.Bounds()
	width := uint32(bounds.Dx())
	height := uint32(bounds.Dy())
	stride := width * 4
	dataSize := stride * height

	// Clamp the rect table so a runaway count can never overflow the buffer.
	if len(rects) > ipc.MaxHostRenderRects {
		rects = rects[:ipc.MaxHostRenderRects]
	}
	rectCount := uint32(len(rects))
	rectsOffset := ipc.SharedRenderHeaderSize + dataSize // table follows the pixels
	rectsBytes := rectCount * ipc.HostRenderHitRectSize

	// Check if header + pixels + rect table fit
	totalSize := rectsOffset + rectsBytes
	if totalSize > sm.size {
		return fmt.Errorf("frame too large: %d bytes (max %d)", totalSize, sm.size)
	}

	sm.sequence++

	// Write header (64 bytes)
	headerBuf := make([]byte, ipc.SharedRenderHeaderSize)
	binary.LittleEndian.PutUint32(headerBuf[0:4], ipc.SharedRenderMagic)
	binary.LittleEndian.PutUint32(headerBuf[4:8], ipc.SharedRenderVersion)
	binary.LittleEndian.PutUint32(headerBuf[8:12], sm.sequence)
	binary.LittleEndian.PutUint32(headerBuf[12:16], ipc.SharedFlagVisible|ipc.SharedFlagContentReady)
	binary.LittleEndian.PutUint32(headerBuf[16:20], uint32(int32(screenX)))
	binary.LittleEndian.PutUint32(headerBuf[20:24], uint32(int32(screenY)))
	binary.LittleEndian.PutUint32(headerBuf[24:28], width)
	binary.LittleEndian.PutUint32(headerBuf[28:32], height)
	binary.LittleEndian.PutUint32(headerBuf[32:36], stride)
	binary.LittleEndian.PutUint32(headerBuf[36:40], dataSize)
	binary.LittleEndian.PutUint32(headerBuf[40:44], rectCount)
	binary.LittleEndian.PutUint32(headerBuf[44:48], rectsOffset)
	binary.LittleEndian.PutUint32(headerBuf[48:52], uint32(int32(renderedHover)))
	// reserved bytes [52:64] stay zero

	dst := unsafe.Slice((*byte)(sm.pView), totalSize)

	// Copy header
	copy(dst[:ipc.SharedRenderHeaderSize], headerBuf)

	// Write BGRA pixels (RGBA → BGRA swap)
	pixelCount := int(width * height)
	pixelDst := dst[ipc.SharedRenderHeaderSize:]
	for i := 0; i < pixelCount; i++ {
		srcIdx := i * 4
		dstIdx := i * 4
		pixelDst[dstIdx+0] = img.Pix[srcIdx+2] // B
		pixelDst[dstIdx+1] = img.Pix[srcIdx+1] // G
		pixelDst[dstIdx+2] = img.Pix[srcIdx+0] // R
		pixelDst[dstIdx+3] = img.Pix[srcIdx+3] // A
	}

	// Write the hit-rect table right after the pixels (panel-local int32 fields).
	rectDst := dst[rectsOffset:totalSize]
	for i, r := range rects {
		off := i * ipc.HostRenderHitRectSize
		binary.LittleEndian.PutUint32(rectDst[off:off+4], uint32(r.Index))
		binary.LittleEndian.PutUint32(rectDst[off+4:off+8], uint32(r.X))
		binary.LittleEndian.PutUint32(rectDst[off+8:off+12], uint32(r.Y))
		binary.LittleEndian.PutUint32(rectDst[off+12:off+16], uint32(r.W))
		binary.LittleEndian.PutUint32(rectDst[off+16:off+20], uint32(r.H))
	}

	return nil
}

// WriteHide writes a "hide" command (flags=0) to shared memory.
// Does NOT signal — the caller wakes the active process's render thread.
func (sm *SharedMemory) WriteHide() {
	sm.mu.Lock()
	defer sm.mu.Unlock()

	if sm.pView == nil {
		return
	}

	sm.sequence++

	// Write minimal header: magic, version, sequence, flags=0 (not visible)
	headerBuf := make([]byte, ipc.SharedRenderHeaderSize)
	binary.LittleEndian.PutUint32(headerBuf[0:4], ipc.SharedRenderMagic)
	binary.LittleEndian.PutUint32(headerBuf[4:8], ipc.SharedRenderVersion)
	binary.LittleEndian.PutUint32(headerBuf[8:12], sm.sequence)
	// flags = 0 (not visible, no content)
	// renderedHoverIndex = -1 (nothing highlighted); the DLL hides without reading it,
	// but keep it consistent so a stale 0 can't read as "candidate 0 highlighted".
	binary.LittleEndian.PutUint32(headerBuf[48:52], 0xFFFFFFFF) // int32(-1) bit pattern

	dst := unsafe.Slice((*byte)(sm.pView), ipc.SharedRenderHeaderSize)
	copy(dst, headerBuf)
}

// Close releases the shared memory mapping.
func (sm *SharedMemory) Close() {
	sm.mu.Lock()
	defer sm.mu.Unlock()

	if sm.pView != nil {
		procUnmapViewOfFile.Call(uintptr(sm.pView))
		sm.pView = nil
	}
	if sm.hMapping != 0 {
		windows.CloseHandle(sm.hMapping)
		sm.hMapping = 0
	}
}

// Name returns the shared memory name.
func (sm *SharedMemory) Name() string { return sm.name }

// Size returns the total shared memory size.
func (sm *SharedMemory) Size() uint32 { return sm.size }

// NamedEvent is a per-process auto-reset wake event for host render. Go signals
// ONLY the active process's event, so a backgrounded process's render thread stays
// asleep — this is what prevents cross-talk when multiple host processes share the
// single global SharedMemory section.
type NamedEvent struct {
	name   string
	handle windows.Handle
}

// newNamedEvent creates a named auto-reset event with the same AppContainer-
// accessible security as the shared memory section.
// name: e.g. "Local\\WindInput_EVT_12345"
func newNamedEvent(name string) (*NamedEvent, error) {
	namePtr, err := windows.UTF16PtrFromString(name)
	if err != nil {
		return nil, fmt.Errorf("invalid event name: %w", err)
	}

	sa := hostRenderSecurityAttributes()
	hEvent, _, err := procCreateEventW.Call(
		uintptr(unsafe.Pointer(sa)),
		0, // auto-reset
		0, // initially non-signaled
		uintptr(unsafe.Pointer(namePtr)),
	)
	if hEvent == 0 {
		return nil, fmt.Errorf("CreateEvent failed: %w", err)
	}

	return &NamedEvent{name: name, handle: windows.Handle(hEvent)}, nil
}

// Signal wakes the render thread waiting on this event (SetEvent). No-op if closed.
func (e *NamedEvent) Signal() {
	if e != nil && e.handle != 0 {
		procSetEvent.Call(uintptr(e.handle))
	}
}

// Close releases the event handle.
func (e *NamedEvent) Close() {
	if e != nil && e.handle != 0 {
		windows.CloseHandle(e.handle)
		e.handle = 0
	}
}

// Name returns the event name.
func (e *NamedEvent) Name() string {
	if e == nil {
		return ""
	}
	return e.name
}
