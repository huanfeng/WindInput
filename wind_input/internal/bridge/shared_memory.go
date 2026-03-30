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
	procFlushViewOfFile    = modkernel32.NewProc("FlushViewOfFile")
)

const (
	fileMapAllAccess = 0xF001F
	pageReadWrite    = 0x04
)

// SharedMemory manages a named shared memory region for host render bitmap transfer.
type SharedMemory struct {
	mu       sync.Mutex
	name     string
	size     uint32
	hMapping windows.Handle
	pView    uintptr
	hEvent   windows.Handle
	evtName  string
	sequence uint32
}

// NewSharedMemory creates a named shared memory region and a named event for signaling.
// name: e.g. "Local\\WindInput_SHM_12345"
// evtName: e.g. "Local\\WindInput_EVT_12345"
// size: total size including header (e.g. MaxSharedRenderSize)
func NewSharedMemory(name, evtName string, size uint32) (*SharedMemory, error) {
	namePtr, err := windows.UTF16PtrFromString(name)
	if err != nil {
		return nil, fmt.Errorf("invalid shared memory name: %w", err)
	}

	// Use same SDDL as the named pipe server — verified to work with AppContainer
	// processes (SearchHost.exe, Start Menu). GA = Generic All access.
	// S:(ML;;NW;;;LW) = Low mandatory label, required for UWP/AppContainer processes.
	sddl := "D:P(A;;GA;;;WD)(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AC)S:(ML;;NW;;;LW)"
	sd, _ := windows.SecurityDescriptorFromString(sddl)
	var sa *windows.SecurityAttributes
	if sd != nil {
		sa = &windows.SecurityAttributes{
			Length:             uint32(unsafe.Sizeof(windows.SecurityAttributes{})),
			SecurityDescriptor: sd,
			InheritHandle:      0,
		}
	}

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
	pView, _, err := procMapViewOfFile.Call(
		hMapping,
		fileMapAllAccess,
		0, 0, // offset
		uintptr(size),
	)
	if pView == 0 {
		windows.CloseHandle(windows.Handle(hMapping))
		return nil, fmt.Errorf("MapViewOfFile failed: %w", err)
	}

	// Zero the header
	headerSlice := unsafe.Slice((*byte)(unsafe.Pointer(pView)), ipc.SharedRenderHeaderSize)
	for i := range headerSlice {
		headerSlice[i] = 0
	}

	// Create named event (auto-reset, initially non-signaled) with same security
	evtNamePtr, _ := windows.UTF16PtrFromString(evtName)
	hEvent, _, err := procCreateEventW.Call(
		uintptr(unsafe.Pointer(sa)),
		0, // auto-reset
		0, // initially non-signaled
		uintptr(unsafe.Pointer(evtNamePtr)),
	)
	if hEvent == 0 {
		procUnmapViewOfFile.Call(pView)
		windows.CloseHandle(windows.Handle(hMapping))
		return nil, fmt.Errorf("CreateEvent failed: %w", err)
	}

	return &SharedMemory{
		name:     name,
		size:     size,
		hMapping: windows.Handle(hMapping),
		pView:    pView,
		hEvent:   windows.Handle(hEvent),
		evtName:  evtName,
	}, nil
}

// WriteFrame writes a rendered candidate image to shared memory and signals the event.
// img must be *image.RGBA. Performs RGBA→BGRA conversion inline.
func (sm *SharedMemory) WriteFrame(img *image.RGBA, screenX, screenY int) error {
	sm.mu.Lock()
	defer sm.mu.Unlock()

	if sm.pView == 0 {
		return fmt.Errorf("shared memory not mapped")
	}

	bounds := img.Bounds()
	width := uint32(bounds.Dx())
	height := uint32(bounds.Dy())
	stride := width * 4
	dataSize := stride * height

	// Check if data fits
	if ipc.SharedRenderHeaderSize+dataSize > sm.size {
		return fmt.Errorf("frame too large: %d bytes (max %d)", ipc.SharedRenderHeaderSize+dataSize, sm.size)
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
	// reserved bytes [40:64] stay zero

	dst := unsafe.Slice((*byte)(unsafe.Pointer(sm.pView)), ipc.SharedRenderHeaderSize+dataSize)

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

	// Signal event to wake DLL render thread
	procSetEvent.Call(uintptr(sm.hEvent))

	return nil
}

// WriteHide writes a "hide" command to shared memory and signals the event.
func (sm *SharedMemory) WriteHide() {
	sm.mu.Lock()
	defer sm.mu.Unlock()

	if sm.pView == 0 {
		return
	}

	sm.sequence++

	// Write minimal header: magic, version, sequence, flags=0 (not visible)
	headerBuf := make([]byte, ipc.SharedRenderHeaderSize)
	binary.LittleEndian.PutUint32(headerBuf[0:4], ipc.SharedRenderMagic)
	binary.LittleEndian.PutUint32(headerBuf[4:8], ipc.SharedRenderVersion)
	binary.LittleEndian.PutUint32(headerBuf[8:12], sm.sequence)
	// flags = 0 (not visible, no content)

	dst := unsafe.Slice((*byte)(unsafe.Pointer(sm.pView)), ipc.SharedRenderHeaderSize)
	copy(dst, headerBuf)

	procSetEvent.Call(uintptr(sm.hEvent))
}

// Close releases all shared memory and event resources.
func (sm *SharedMemory) Close() {
	sm.mu.Lock()
	defer sm.mu.Unlock()

	if sm.pView != 0 {
		procUnmapViewOfFile.Call(sm.pView)
		sm.pView = 0
	}
	if sm.hMapping != 0 {
		windows.CloseHandle(sm.hMapping)
		sm.hMapping = 0
	}
	if sm.hEvent != 0 {
		windows.CloseHandle(sm.hEvent)
		sm.hEvent = 0
	}
}

// Name returns the shared memory name.
func (sm *SharedMemory) Name() string { return sm.name }

// EventName returns the event name.
func (sm *SharedMemory) EventName() string { return sm.evtName }

// Size returns the total shared memory size.
func (sm *SharedMemory) Size() uint32 { return sm.size }
