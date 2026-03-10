// Package coordinator orchestrates communication between C++ Bridge, Engine, and UI
package coordinator

import (
	"log/slog"
	"sync"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/engine"
	"github.com/huanfeng/wind_input/internal/hotkey"
	"github.com/huanfeng/wind_input/internal/transform"
	"github.com/huanfeng/wind_input/internal/ui"
	"github.com/huanfeng/wind_input/pkg/config"
)

// Restart request channel - main should listen to this
var restartRequestCh = make(chan struct{}, 1)

// RequestRestart signals that a restart is requested
func RequestRestart() {
	select {
	case restartRequestCh <- struct{}{}:
	default:
		// Channel already has a request pending
	}
}

// RestartRequested returns a channel that signals when restart is requested
func RestartRequested() <-chan struct{} {
	return restartRequestCh
}

// Exit request channel - main should listen to this
var exitRequestCh = make(chan struct{}, 1)

// RequestExit signals that an application exit is requested
func RequestExit() {
	select {
	case exitRequestCh <- struct{}{}:
	default:
		// Channel already has a request pending
	}
}

// ExitRequested returns a channel that signals when exit is requested
func ExitRequested() <-chan struct{} {
	return exitRequestCh
}

// Modifier key flags (must match C++ side)
const (
	ModShift = 0x01
	ModCtrl  = 0x02
	ModAlt   = 0x04
)

// EffectiveMode represents the effective input mode considering CapsLock
type EffectiveMode int

const (
	ModeChinese      EffectiveMode = iota // 中文模式
	ModeEnglishLower                      // 英文小写模式
	ModeEnglishUpper                      // 英文大写模式 (CapsLock on)
)

// Coordinator orchestrates between C++ Bridge, Engine, and native UI
type Coordinator struct {
	engineMgr    *engine.Manager
	uiManager    *ui.Manager
	logger       *slog.Logger
	config       *config.Config
	bridgeServer BridgeServer // Interface for broadcasting state to TSF clients

	mu sync.Mutex

	// Input mode state
	chineseMode bool // true = Chinese, false = English
	capsLockOn  bool // CapsLock state (authority source)

	// Full-width and punctuation state
	fullWidth          bool // true = full-width, false = half-width
	chinesePunctuation bool // true = Chinese punctuation, false = English punctuation
	punctFollowMode    bool // true = punctuation follows Chinese/English mode
	toolbarVisible     bool // true = toolbar visible
	imeActivated       bool // true = IME is activated (has focus)

	// Input state
	inputBuffer        string
	inputCursorPos     int    // 光标在 inputBuffer 中的字节位置（0 = 最左，len(inputBuffer) = 最右）
	preeditDisplay     string // 带音节分隔符的显示文本（如 "zhong'guo"），五笔时为空
	syllableBoundaries []int  // 音节边界在 inputBuffer 中的位置（如 [5] 表示位置 5 处有分隔符）
	candidates         []ui.Candidate
	currentPage        int
	totalPages         int
	candidatesPerPage  int

	// 临时英文模式状态
	tempEnglishMode   bool   // 是否处于临时英文模式
	tempEnglishBuffer string // 临时英文缓冲区

	// 临时拼音模式状态
	tempPinyinMode   bool   // 是否处于临时拼音模式
	tempPinyinBuffer string // 临时拼音输入缓冲区

	// Caret position (from C++)
	caretX      int
	caretY      int
	caretHeight int
	caretValid  bool // true if we have received valid caret position (coordinates can be negative in multi-monitor)

	// Last known valid window position (for fallback)
	lastValidX int
	lastValidY int

	// Punctuation converter with state (for paired punctuation like quotes)
	punctConverter *transform.PunctuationConverter

	// Hotkey compiler for binary protocol
	hotkeyCompiler *hotkey.Compiler

	// 热键缓存（避免每次焦点变化重新编译）
	cachedKeyDownHotkeys []uint32
	cachedKeyUpHotkeys   []uint32
	hotkeysDirty         bool // 配置变化时置 true
}

// BridgeServer interface for broadcasting state to TSF clients
type BridgeServer interface {
	PushStateToAllClients(status *bridge.StatusUpdateData)
	PushCommitTextToActiveClient(text string) // Only send to active client for security
	PushClearCompositionToActiveClient()      // Clear inline composition on active client
	RestartService()
}

// SetBridgeServer sets the bridge server for state broadcasting
func (c *Coordinator) SetBridgeServer(server BridgeServer) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.bridgeServer = server
}

// GetEffectiveMode returns the effective input mode considering CapsLock
// - Chinese mode + CapsLock OFF = Chinese
// - Chinese mode + CapsLock ON = English Upper (temporary English for caps)
// - English mode + CapsLock OFF = English Lower
// - English mode + CapsLock ON = English Upper
func (c *Coordinator) GetEffectiveMode() EffectiveMode {
	if c.capsLockOn {
		return ModeEnglishUpper
	}
	if c.chineseMode {
		return ModeChinese
	}
	return ModeEnglishLower
}

// GetEffectiveModeNoLock returns the effective input mode without acquiring lock
// Caller must hold the lock
func (c *Coordinator) getEffectiveModeNoLock() EffectiveMode {
	if c.capsLockOn {
		return ModeEnglishUpper
	}
	if c.chineseMode {
		return ModeChinese
	}
	return ModeEnglishLower
}

// IsCapsLockOn returns the current CapsLock state
func (c *Coordinator) IsCapsLockOn() bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.capsLockOn
}

// buildStatusUpdate creates a StatusUpdateData from current state (caller must hold lock)
func (c *Coordinator) buildStatusUpdate() *bridge.StatusUpdateData {
	keyDownHotkeys, keyUpHotkeys := c.getCompiledHotkeys()
	return &bridge.StatusUpdateData{
		ChineseMode:        c.chineseMode,
		FullWidth:          c.fullWidth,
		ChinesePunctuation: c.chinesePunctuation,
		ToolbarVisible:     c.toolbarVisible,
		CapsLock:           c.capsLockOn,
		KeyDownHotkeys:     keyDownHotkeys,
		KeyUpHotkeys:       keyUpHotkeys,
	}
}

// broadcastState broadcasts the current state to toolbar and all TSF clients
// This should be called after any state change. Caller must hold the lock.
func (c *Coordinator) broadcastState() {
	// 1. Update Go toolbar
	c.syncToolbarStateNoLock()

	// 2. Push state to all TSF clients
	if c.bridgeServer != nil {
		status := c.buildStatusUpdate()
		// Release lock before network I/O to avoid blocking
		c.mu.Unlock()
		c.bridgeServer.PushStateToAllClients(status)
		c.mu.Lock()
	}
}

// syncToolbarStateNoLock synchronizes the current state to the toolbar (without lock)
func (c *Coordinator) syncToolbarStateNoLock() {
	if c.uiManager == nil {
		return
	}

	// Use effective mode for toolbar display
	effectiveMode := c.getEffectiveModeNoLock()

	c.uiManager.UpdateToolbarState(ui.ToolbarState{
		ChineseMode:   effectiveMode == ModeChinese,
		FullWidth:     c.fullWidth,
		ChinesePunct:  c.chinesePunctuation,
		CapsLock:      c.capsLockOn,
		EffectiveMode: int(effectiveMode),
	})
}

// NewCoordinator creates a new Coordinator
func NewCoordinator(engineMgr *engine.Manager, uiManager *ui.Manager, cfg *config.Config, logger *slog.Logger) *Coordinator {
	candidatesPerPage := 9
	if cfg != nil && cfg.UI.CandidatesPerPage > 0 {
		candidatesPerPage = cfg.UI.CandidatesPerPage
	}

	// 确定初始状态
	startInChineseMode := true
	fullWidth := false
	chinesePunctuation := true
	punctFollowMode := false
	toolbarVisible := false

	if cfg != nil {
		// 检查是否启用"记忆前次状态"
		if cfg.Startup.RememberLastState {
			// 从 RuntimeState 加载前次状态
			state, err := config.LoadRuntimeState()
			if err != nil {
				logger.Warn("Failed to load runtime state, using defaults", "error", err)
				startInChineseMode = cfg.Startup.DefaultChineseMode
				fullWidth = cfg.Startup.DefaultFullWidth
				chinesePunctuation = cfg.Startup.DefaultChinesePunct
			} else {
				startInChineseMode = state.ChineseMode
				fullWidth = state.FullWidth
				chinesePunctuation = state.ChinesePunct
			}
		} else {
			// 使用默认配置
			startInChineseMode = cfg.Startup.DefaultChineseMode
			fullWidth = cfg.Startup.DefaultFullWidth
			chinesePunctuation = cfg.Startup.DefaultChinesePunct
		}

		punctFollowMode = cfg.Input.PunctFollowMode
		toolbarVisible = cfg.Toolbar.Visible
	}

	c := &Coordinator{
		engineMgr:          engineMgr,
		uiManager:          uiManager,
		logger:             logger,
		config:             cfg,
		chineseMode:        startInChineseMode,
		fullWidth:          fullWidth,
		chinesePunctuation: chinesePunctuation,
		punctFollowMode:    punctFollowMode,
		toolbarVisible:     toolbarVisible,
		inputBuffer:        "",
		candidates:         nil,
		currentPage:        1,
		totalPages:         1,
		candidatesPerPage:  candidatesPerPage,
		caretX:             100,
		caretY:             100,
		caretHeight:        20,
		punctConverter:     transform.NewPunctuationConverter(),
		hotkeyCompiler:     hotkey.NewCompiler(cfg),
		hotkeysDirty:       true, // 首次使用时需要编译
	}

	// Set up toolbar callbacks
	c.setupToolbarCallbacks()

	// Set up candidate window callbacks for mouse interaction
	c.setupCandidateCallbacks()

	// Initialize UI config (including debug options)
	if c.uiManager != nil && cfg != nil {
		c.uiManager.UpdateConfig(cfg.UI.FontSize, cfg.UI.FontPath, cfg.UI.HideCandidateWindow)
		// Set candidate layout (horizontal/vertical)
		if cfg.UI.CandidateLayout != "" {
			c.uiManager.SetCandidateLayout(cfg.UI.CandidateLayout)
		}
		// Set hide preedit when inline preedit is enabled
		c.uiManager.SetHidePreedit(cfg.UI.InlinePreedit)
		// Set status indicator config
		c.uiManager.UpdateStatusIndicatorConfig(
			cfg.UI.StatusIndicatorDuration,
			cfg.UI.StatusIndicatorOffsetX,
			cfg.UI.StatusIndicatorOffsetY,
		)
		// 设置编码提示延迟
		c.uiManager.SetTooltipDelay(cfg.UI.TooltipDelay)
		// 加载主题
		if cfg.UI.Theme != "" {
			c.uiManager.LoadTheme(cfg.UI.Theme)
		}
	}

	return c
}
func (c *Coordinator) clearState() {
	c.inputBuffer = ""
	c.inputCursorPos = 0
	c.preeditDisplay = ""
	c.syllableBoundaries = nil
	c.tempEnglishMode = false
	c.tempEnglishBuffer = ""
	c.tempPinyinMode = false
	c.tempPinyinBuffer = ""
	c.candidates = nil
	c.currentPage = 1
	c.totalPages = 1
}
