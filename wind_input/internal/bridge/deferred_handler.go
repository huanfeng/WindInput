package bridge

import (
	"log/slog"
	"sync"
	"sync/atomic"
)

// DeferredHandler implements MessageHandler as a proxy that returns safe defaults
// while the real handler (Coordinator) is still initializing.
//
// On first install, wdb file generation can take several seconds. Without this,
// the named pipe wouldn't exist until initialization completes, causing the TSF
// DLL to block the host process (e.g., Notepad) during OnSetFocus.
//
// With DeferredHandler, the bridge server starts immediately with the pipe available.
// TSF clients connect successfully and receive a "loading" status (iconLabel "…"),
// while key events pass through to the application unblocked.
type DeferredHandler struct {
	ready   atomic.Bool
	mu      sync.RWMutex
	handler MessageHandler
	logger  *slog.Logger
}

// NewDeferredHandler creates a handler proxy that returns safe defaults until SetReady is called.
func NewDeferredHandler(logger *slog.Logger) *DeferredHandler {
	return &DeferredHandler{
		logger: logger,
	}
}

// SetReady swaps in the real handler and marks the proxy as ready.
// After this call, all requests are delegated to the real handler.
func (d *DeferredHandler) SetReady(handler MessageHandler) {
	d.mu.Lock()
	d.handler = handler
	d.mu.Unlock()
	d.ready.Store(true)
	d.logger.Info("DeferredHandler is now ready, delegating to real handler")
}

// getHandler returns the real handler if ready, nil otherwise.
func (d *DeferredHandler) getHandler() MessageHandler {
	if !d.ready.Load() {
		return nil
	}
	d.mu.RLock()
	h := d.handler
	d.mu.RUnlock()
	return h
}

// loadingStatus returns a StatusUpdateData with a "…" icon label to indicate loading state.
func (d *DeferredHandler) loadingStatus() *StatusUpdateData {
	return &StatusUpdateData{
		ChineseMode: false,
		IconLabel:   "…",
	}
}

// --- MessageHandler interface implementation ---

func (d *DeferredHandler) HandleKeyEvent(data KeyEventData) *KeyEventResult {
	if h := d.getHandler(); h != nil {
		return h.HandleKeyEvent(data)
	}
	// Not ready: pass through all keys to the application
	return nil
}

func (d *DeferredHandler) HandleCaretUpdate(data CaretData) error {
	if h := d.getHandler(); h != nil {
		return h.HandleCaretUpdate(data)
	}
	return nil
}

func (d *DeferredHandler) HandleCaretPending() {
	if h := d.getHandler(); h != nil {
		h.HandleCaretPending()
	}
}

func (d *DeferredHandler) HandleFocusLost() {
	if h := d.getHandler(); h != nil {
		h.HandleFocusLost()
	}
}

func (d *DeferredHandler) HandleCompositionTerminated() {
	if h := d.getHandler(); h != nil {
		h.HandleCompositionTerminated()
	}
}

func (d *DeferredHandler) HandleFocusGained(processID uint32, inputScopeMask uint64) *StatusUpdateData {
	if h := d.getHandler(); h != nil {
		return h.HandleFocusGained(processID, inputScopeMask)
	}
	d.logger.Debug("Service initializing, returning loading status for FocusGained", "processID", processID)
	return d.loadingStatus()
}

func (d *DeferredHandler) HandleIMEDeactivated() {
	if h := d.getHandler(); h != nil {
		h.HandleIMEDeactivated()
	}
}

func (d *DeferredHandler) HandleIMEActivated(processID uint32) *StatusUpdateData {
	if h := d.getHandler(); h != nil {
		return h.HandleIMEActivated(processID)
	}
	d.logger.Debug("Service initializing, returning loading status for IMEActivated", "processID", processID)
	return d.loadingStatus()
}

func (d *DeferredHandler) HandleToggleMode() (status *StatusUpdateData, commitText string) {
	if h := d.getHandler(); h != nil {
		return h.HandleToggleMode()
	}
	return d.loadingStatus(), ""
}

func (d *DeferredHandler) HandleCapsLockState(on bool) {
	if h := d.getHandler(); h != nil {
		h.HandleCapsLockState(on)
	}
}

func (d *DeferredHandler) HandleMenuCommand(command string) *StatusUpdateData {
	if h := d.getHandler(); h != nil {
		return h.HandleMenuCommand(command)
	}
	return d.loadingStatus()
}

func (d *DeferredHandler) HandleClientDisconnected(activeClients int) {
	if h := d.getHandler(); h != nil {
		h.HandleClientDisconnected(activeClients)
	}
}

func (d *DeferredHandler) HandleCommitRequest(data CommitRequestData) *CommitResultData {
	if h := d.getHandler(); h != nil {
		return h.HandleCommitRequest(data)
	}
	return nil
}

func (d *DeferredHandler) HandleModeNotify(data ModeNotifyData) {
	if h := d.getHandler(); h != nil {
		h.HandleModeNotify(data)
	}
}

func (d *DeferredHandler) HandleSystemModeSwitch(chineseMode bool) (status *StatusUpdateData, commitText string) {
	if h := d.getHandler(); h != nil {
		return h.HandleSystemModeSwitch(chineseMode)
	}
	return d.loadingStatus(), ""
}

func (d *DeferredHandler) HandleShowContextMenu(screenX, screenY int) {
	if h := d.getHandler(); h != nil {
		h.HandleShowContextMenu(screenX, screenY)
	}
}

func (d *DeferredHandler) HandleSelectionChanged(prevChar rune) {
	if h := d.getHandler(); h != nil {
		h.HandleSelectionChanged(prevChar)
	}
}

func (d *DeferredHandler) HandleHostRenderReady() {
	if h := d.getHandler(); h != nil {
		h.HandleHostRenderReady()
	}
}

func (d *DeferredHandler) HandleInputStats(chars, digits, puncts, spaces, elapsedMs int) {
	if h := d.getHandler(); h != nil {
		h.HandleInputStats(chars, digits, puncts, spaces, elapsedMs)
	}
}

func (d *DeferredHandler) GetCurrentMode() (chineseMode bool, fullWidth bool) {
	if h := d.getHandler(); h != nil {
		return h.GetCurrentMode()
	}
	// 初始化期间使用安全默认值：中文模式，半角（与 _bChineseMode=TRUE 初始化一致）
	return true, false
}

// HandleCandidateSelect 转发给底层 handler (若其实现 candidateSelector, Coordinator 实现)。
// 不在 MessageHandler 接口内, 仅为 darwin 鼠标点选可选扩展。
func (d *DeferredHandler) HandleCandidateSelect(index int) {
	if cs, ok := d.getHandler().(candidateSelector); ok {
		cs.HandleCandidateSelect(index)
	}
}

// HandleCandidateContextMenu 转发右键菜单动作给底层 handler (Coordinator 实现)。
func (d *DeferredHandler) HandleCandidateContextMenu(index int, action string) {
	if h, ok := d.getHandler().(candidateContextMenuHandler); ok {
		h.HandleCandidateContextMenu(index, action)
	}
}

// HandleCandidateHover 转发候选悬停给底层 handler 触发 tooltip 查询 (Coordinator 实现)。
func (d *DeferredHandler) HandleCandidateHover(index int) {
	if h, ok := d.getHandler().(candidateHoverHandler); ok {
		h.HandleCandidateHover(index)
	}
}

// HandleCandidateHoverAt 转发带屏幕锚点的候选悬停 (Windows host render, Coordinator 实现)。
func (d *DeferredHandler) HandleCandidateHoverAt(index, tooltipX, tooltipBelowY, tooltipAboveY int) {
	if h, ok := d.getHandler().(hostCandidateHoverHandler); ok {
		h.HandleCandidateHoverAt(index, tooltipX, tooltipBelowY, tooltipAboveY)
	}
}

// HandleCandidateScroll 转发候选框鼠标滚轮 (Windows host render, Coordinator 实现)。
func (d *DeferredHandler) HandleCandidateScroll(delta int) {
	if h, ok := d.getHandler().(candidateScrollHandler); ok {
		h.HandleCandidateScroll(delta)
	}
}

// HandleHostRenderFailed 转发 host render 建窗失败上报 (Windows, Coordinator 实现)。
func (d *DeferredHandler) HandleHostRenderFailed(processID uint32, reason uint32) {
	if h, ok := d.getHandler().(hostRenderFailureReporter); ok {
		h.HandleHostRenderFailed(processID, reason)
	}
}

// UnifiedMenuItems 转发: 取底层 handler 构建的统一菜单树 (Coordinator 实现)。
func (d *DeferredHandler) UnifiedMenuItems() []MenuItem {
	if h, ok := d.getHandler().(unifiedMenuHandler); ok {
		return h.UnifiedMenuItems()
	}
	return nil
}

// HandleUnifiedMenuAction 转发统一菜单动作给底层 handler。
func (d *DeferredHandler) HandleUnifiedMenuAction(id int) {
	if h, ok := d.getHandler().(unifiedMenuHandler); ok {
		h.HandleUnifiedMenuAction(id)
	}
}
