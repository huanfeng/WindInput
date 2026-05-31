//go:build darwin

package bridge

import (
	"encoding/binary"
	"fmt"
	"image"
	"log/slog"
	"sync"
	"sync/atomic"
	"syscall"
	"unsafe"

	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/pkg/buildvariant"
	"golang.org/x/sys/unix"
)

// shmOpen 调用 darwin 内核 shm_open(name, oflag, mode) syscall (SYS_SHM_OPEN=266)。
// x/sys/unix darwin 未导出 ShmOpen 包装, 这里手工 syscall。name 是 C 字符串
// (null 结尾), POSIX 规定以 "/" 开头, macOS 长度 ≤ 31 (含 NUL)。
func shmOpen(name string, oflag int, mode uint32) (int, error) {
	cname, err := syscall.BytePtrFromString(name)
	if err != nil {
		return -1, err
	}
	r1, _, errno := syscall.Syscall(unix.SYS_SHM_OPEN,
		uintptr(unsafe.Pointer(cname)),
		uintptr(oflag),
		uintptr(mode))
	if errno != 0 {
		return -1, errno
	}
	return int(r1), nil
}

// shmUnlink 调用 shm_unlink(name) syscall (SYS_SHM_UNLINK=267)。
func shmUnlink(name string) error {
	cname, err := syscall.BytePtrFromString(name)
	if err != nil {
		return err
	}
	_, _, errno := syscall.Syscall(unix.SYS_SHM_UNLINK,
		uintptr(unsafe.Pointer(cname)), 0, 0)
	if errno != 0 {
		return errno
	}
	return nil
}

// host_render_darwin.go — darwin 版 host-render: POSIX SHM (shm_open + mmap)
// 镜像 Win 端 CreateFileMappingW + MapViewOfFile 路径。
//
// 与 Win 模型的关键差异:
//   - **单消费者**: macOS 上 IMKit `.app` 是唯一的渲染消费者 (它的 NSPanel 替所有
//     目标应用显示候选框, 不像 Win 端那样要给每个 host app 单独 inject SHM)。
//     因此本端用全局单段 SHM ("/WindInput_SHM"), 无 per-PID 分桶。
//   - **无 named Event 通知**: POSIX SHM 无对应 Win named Event API。改用 bridge
//     push 通道 (现有 UDS) 发短帧 "ready, seq=N" 通知 IMKit; IMKit 收到后从 SHM
//     拉最新帧。
//   - **大小约束**: macOS POSIX SHM 默认上限通常 ~4 MB, 与 ipc.MaxSharedRenderSize
//     恰好对齐, 当前足够 ~1024×1024 BGRA。
//
// 协议同步铁律: SHM header 二进制布局必须与 Win 端 (shared_memory.go WriteFrame
// 部分) 完全一致, 由 ipc.SharedRenderHeaderSize + 6 个 u32 字段固定。

// 变体后缀隔离 SHM 段 (release: /WindInput_SHM; debug: /WindInput_SHM_debug)。
// 否则开机后两变体服务都自启, NewSharedMemory 起手的 shmUnlink 会互相清掉对方的段,
// 候选框渲染坏掉。≤30 字符 (macOS PSHMNAMLEN=31): "_debug" 后仍 20 字符, 安全。
var darwinSHMName = "/WindInput_SHM" + buildvariant.Suffix()

// SharedMemory — POSIX shm_open + mmap 封装。
type SharedMemory struct {
	mu       sync.Mutex
	name     string
	size     uint32
	fd       int
	bytes    []byte // mmap 后的 view, 直接索引
	sequence uint32
}

// NewSharedMemory 创建/打开命名 POSIX SHM 段并 mmap。
// name 必须以 "/" 开头 (POSIX 规范), 长度 ≤ 30 (macOS PSHMNAMLEN=31 含 NUL)。
func NewSharedMemory(name string, size uint32) (*SharedMemory, error) {
	// 先清理可能残留的同名段 (上次进程异常退出未 unlink), 忽略错误
	_ = shmUnlink(name)

	fd, err := shmOpen(name, unix.O_CREAT|unix.O_RDWR|unix.O_EXCL, 0o600)
	if err != nil {
		return nil, fmt.Errorf("shm_open(%s): %w", name, err)
	}
	if err := unix.Ftruncate(fd, int64(size)); err != nil {
		_ = unix.Close(fd)
		_ = shmUnlink(name)
		return nil, fmt.Errorf("ftruncate(%d): %w", size, err)
	}
	b, err := unix.Mmap(fd, 0, int(size), unix.PROT_READ|unix.PROT_WRITE, unix.MAP_SHARED)
	if err != nil {
		_ = unix.Close(fd)
		_ = shmUnlink(name)
		return nil, fmt.Errorf("mmap: %w", err)
	}
	return &SharedMemory{
		name:  name,
		size:  size,
		fd:    fd,
		bytes: b,
	}, nil
}

// WriteFrame 把一张 RGBA 候选框图写入 SHM, RGBA→BGRA inline 转换 (与 Win 同协议)。
// screenX/screenY = 候选框左上角屏幕坐标 (top-left, wire 坐标系)。
// 返回写入的 sequence (调用方应通过 bridge push 通知客户端此 seq)。
func (sm *SharedMemory) WriteFrame(img *image.RGBA, screenX, screenY int) (uint32, error) {
	sm.mu.Lock()
	defer sm.mu.Unlock()

	if sm.bytes == nil {
		return 0, fmt.Errorf("shared memory not mapped")
	}

	bounds := img.Bounds()
	width := uint32(bounds.Dx())
	height := uint32(bounds.Dy())
	stride := width * 4
	dataSize := stride * height

	if ipc.SharedRenderHeaderSize+dataSize > sm.size {
		return 0, fmt.Errorf("frame too large: %d > %d",
			ipc.SharedRenderHeaderSize+dataSize, sm.size)
	}

	sm.sequence++
	seq := sm.sequence

	// 写 header (64 bytes, 与 Win 端字段顺序完全一致)
	hdr := sm.bytes[:ipc.SharedRenderHeaderSize]
	binary.LittleEndian.PutUint32(hdr[0:4], ipc.SharedRenderMagic)
	binary.LittleEndian.PutUint32(hdr[4:8], ipc.SharedRenderVersion)
	binary.LittleEndian.PutUint32(hdr[8:12], seq)
	binary.LittleEndian.PutUint32(hdr[12:16], ipc.SharedFlagVisible|ipc.SharedFlagContentReady)
	binary.LittleEndian.PutUint32(hdr[16:20], uint32(int32(screenX)))
	binary.LittleEndian.PutUint32(hdr[20:24], uint32(int32(screenY)))
	binary.LittleEndian.PutUint32(hdr[24:28], width)
	binary.LittleEndian.PutUint32(hdr[28:32], height)
	binary.LittleEndian.PutUint32(hdr[32:36], stride)
	binary.LittleEndian.PutUint32(hdr[36:40], dataSize)
	// [40:64] reserved zeros

	// RGBA → BGRA 拷贝 (Win/Mac 客户端都吃 BGRA, GPU blit 原生格式)
	pixelCount := int(width * height)
	dst := sm.bytes[ipc.SharedRenderHeaderSize:]
	for i := 0; i < pixelCount; i++ {
		si := i * 4
		di := i * 4
		dst[di+0] = img.Pix[si+2]
		dst[di+1] = img.Pix[si+1]
		dst[di+2] = img.Pix[si+0]
		dst[di+3] = img.Pix[si+3]
	}
	return seq, nil
}

// WriteHide 写一个"隐藏"标记 (flags=0), 不带像素。
// 返回新 sequence。
func (sm *SharedMemory) WriteHide() uint32 {
	sm.mu.Lock()
	defer sm.mu.Unlock()
	if sm.bytes == nil {
		return 0
	}
	sm.sequence++
	seq := sm.sequence
	hdr := sm.bytes[:ipc.SharedRenderHeaderSize]
	binary.LittleEndian.PutUint32(hdr[0:4], ipc.SharedRenderMagic)
	binary.LittleEndian.PutUint32(hdr[4:8], ipc.SharedRenderVersion)
	binary.LittleEndian.PutUint32(hdr[8:12], seq)
	binary.LittleEndian.PutUint32(hdr[12:16], 0) // flags=0
	for i := 16; i < int(ipc.SharedRenderHeaderSize); i++ {
		hdr[i] = 0
	}
	return seq
}

// Close munmap + close fd + shm_unlink。
func (sm *SharedMemory) Close() {
	sm.mu.Lock()
	defer sm.mu.Unlock()
	if sm.bytes != nil {
		_ = unix.Munmap(sm.bytes)
		sm.bytes = nil
	}
	if sm.fd > 0 {
		_ = unix.Close(sm.fd)
		sm.fd = 0
	}
	if sm.name != "" {
		_ = shmUnlink(sm.name)
		sm.name = ""
	}
}

func (sm *SharedMemory) Name() string      { return sm.name }
func (sm *SharedMemory) EventName() string { return "" } // POSIX 不用 named event
func (sm *SharedMemory) Size() uint32      { return sm.size }

// ============================================================================
// HostRenderManager — darwin 单段 SHM 简化版
// ============================================================================

// HostRenderState — darwin 上仅持 SHM 引用 (无 Win 的 ProcessID/Active/SetupSeq)。
type HostRenderState struct {
	SHM *SharedMemory
}

// HostRenderManager — darwin 单消费者模型: 全局一份 SHM, 不分 PID。
// processNames 白名单参数被忽略 (macOS 不需要 host 进程过滤)。
type HostRenderManager struct {
	mu     sync.Mutex
	logger *slog.Logger
	shm    *SharedMemory
	ready  atomic.Bool
}

func NewHostRenderManager(logger *slog.Logger, processNames []string) *HostRenderManager {
	return &HostRenderManager{logger: logger}
}

func (m *HostRenderManager) UpdateWhitelist(processNames []string) {}

// IsProcessWhitelisted darwin 返回 true (单消费者, 视为永远 whitelisted)。
func (m *HostRenderManager) IsProcessWhitelisted(processID uint32) bool { return true }

// SetupHostRender 懒分配全局 SHM, 返回 setup payload (含 SHM 名)。
// processID 参数被忽略 — darwin 上所有调用共享同一段 SHM。
func (m *HostRenderManager) SetupHostRender(processID uint32) (*ipc.HostRenderSetupPayload, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.shm == nil {
		sm, err := NewSharedMemory(darwinSHMName, ipc.MaxSharedRenderSize)
		if err != nil {
			return nil, fmt.Errorf("setup darwin SHM: %w", err)
		}
		m.shm = sm
		m.ready.Store(true)
		m.logger.Info("darwin host render SHM ready",
			"name", sm.Name(), "size", sm.Size())
	}
	return &ipc.HostRenderSetupPayload{
		MaxBufferSize: m.shm.Size(),
		ShmName:       m.shm.Name(),
		EventName:     "",
	}, nil
}

// GetSetupSeq darwin 上无 SetupSeq 概念, 返回 1 表示 ready (0 表示未 setup)。
func (m *HostRenderManager) GetSetupSeq(processID uint32) uint64 {
	if m.ready.Load() {
		return 1
	}
	return 0
}

// GetActiveState 返回单例 state (含 SHM), shm 未 setup 时返回 nil。
func (m *HostRenderManager) GetActiveState(processID uint32) *HostRenderState {
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.shm == nil {
		return nil
	}
	return &HostRenderState{SHM: m.shm}
}

// CleanupClient darwin 不按 PID 清理 — 单 SHM 复用, 客户端断开仅记日志。
func (m *HostRenderManager) CleanupClient(processID uint32, expectedSeq uint64) {
	m.logger.Debug("darwin host render CleanupClient ignored", "pid", processID)
}

// CleanupAll 在服务退出时调用, 释放 SHM 并 unlink。
func (m *HostRenderManager) CleanupAll() {
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.shm != nil {
		m.shm.Close()
		m.shm = nil
		m.ready.Store(false)
	}
}

// GetProcessName darwin 上不识别 host 进程名 (IMKit `.app` 自报 bundleID 走 IMKit
// attach 帧, 不走 sysctl), 始终返回空字符串。
func GetProcessName(pid uint32) string { return "" }
