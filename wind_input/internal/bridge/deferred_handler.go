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

func (d *DeferredHandler) HandleFocusGained(processID uint32) *StatusUpdateData {
	if h := d.getHandler(); h != nil {
		return h.HandleFocusGained(processID)
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

func (d *DeferredHandler) HandleToggleMode() (commitText string, chineseMode bool) {
	if h := d.getHandler(); h != nil {
		return h.HandleToggleMode()
	}
	return "", false
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

func (d *DeferredHandler) HandleSystemModeSwitch(chineseMode bool) (commitText string) {
	if h := d.getHandler(); h != nil {
		return h.HandleSystemModeSwitch(chineseMode)
	}
	return ""
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
