//go:build darwin

package bridge

import (
	"image"
	"log/slog"

	"github.com/huanfeng/wind_input/internal/ipc"
)

// host_render_darwin.go 提供 HostRenderManager 在 darwin 上的占位实现。
//
// macOS 上 IMKit `.app` 是独立 GUI 进程, 自绘 NSPanel 候选框, 天然浮在所有
// 应用窗口之上 (kCGPopUpMenuWindowLevel), 因此完全不需要 Windows 上为了
// 突破 Band 层级而设计的"宿主进程内 in-proc 渲染 + 共享内存推 bitmap"机制。
//
// 这里所有方法都是 no-op / 返回安全默认值, 让 cmd/service/main.go 与
// coordinator 在 darwin 编译时仍可调用同名 API, 但不会触发任何 host render 行为。

// HostRenderState darwin 上的占位类型 (字段最小化)。
type HostRenderState struct {
	SHM *SharedMemory
}

// HostRenderManager darwin 上的占位类型。
// 不持有任何 Win 资源, 所有 method 都是 no-op。
type HostRenderManager struct {
	logger *slog.Logger
}

// SharedMemory darwin 上的占位类型。
// 调用 WriteFrame/WriteHide 都是 no-op (darwin 不需要 bitmap 跨进程传输)。
type SharedMemory struct{}

func (sm *SharedMemory) WriteFrame(img *image.RGBA, screenX, screenY int) error {
	return nil
}
func (sm *SharedMemory) WriteHide()        {}
func (sm *SharedMemory) Close()            {}
func (sm *SharedMemory) Name() string      { return "" }
func (sm *SharedMemory) EventName() string { return "" }
func (sm *SharedMemory) Size() uint32      { return 0 }

// NewHostRenderManager darwin 上返回一个 no-op manager。
// processNames 参数被忽略 (darwin 没有"宿主进程白名单"概念)。
func NewHostRenderManager(logger *slog.Logger, processNames []string) *HostRenderManager {
	return &HostRenderManager{logger: logger}
}

// UpdateWhitelist no-op on darwin.
func (m *HostRenderManager) UpdateWhitelist(processNames []string) {}

// IsProcessWhitelisted darwin 上始终 false (无白名单概念)。
func (m *HostRenderManager) IsProcessWhitelisted(processID uint32) bool { return false }

// SetupHostRender darwin 上不支持, 返回 nil。
func (m *HostRenderManager) SetupHostRender(processID uint32) (*ipc.HostRenderSetupPayload, error) {
	return nil, nil
}

// GetSetupSeq darwin 上始终 0。
func (m *HostRenderManager) GetSetupSeq(processID uint32) uint64 { return 0 }

// GetActiveState darwin 上始终 nil。
func (m *HostRenderManager) GetActiveState(processID uint32) *HostRenderState { return nil }

// CleanupClient no-op on darwin.
func (m *HostRenderManager) CleanupClient(processID uint32, expectedSeq uint64) {}

// CleanupAll no-op on darwin.
func (m *HostRenderManager) CleanupAll() {}

// GetProcessName darwin 上的占位实现, 始终返回空字符串。
// macOS 端真正用户应用的识别走 IMKInputController.client().bundleIdentifier(),
// 由 IMKit `.app` 端在 attach 帧自报, 不在此处实现。
func GetProcessName(pid uint32) string { return "" }
