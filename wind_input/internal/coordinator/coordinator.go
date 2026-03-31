// Package coordinator orchestrates communication between C++ Bridge, Engine, and UI
package coordinator

import (
	"image"
	"log/slog"
	"sync"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/engine"
	"github.com/huanfeng/wind_input/internal/hotkey"
	"github.com/huanfeng/wind_input/internal/transform"
	"github.com/huanfeng/wind_input/internal/ui"
	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/theme"
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

// ConfirmedSegment 代表拼音分步确认中一个已确认但未上屏的文本段。
// 用户选词后，如果输入缓冲区未完全消费，候选文字暂存于此而非直接上屏，
// 用户可通过退格键回退到上一个确认段重新选词。
type ConfirmedSegment struct {
	Text         string // 已确认的汉字，如 "我们"
	ConsumedCode string // 消耗的原始拼音编码，如 "women"
}

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
	inputCursorPos     int                // 光标在 inputBuffer 中的字节位置（0 = 最左，len(inputBuffer) = 最右）
	preeditDisplay     string             // 带音节分隔符的显示文本（如 "zhong'guo"），五笔时为空
	syllableBoundaries []int              // 音节边界在 inputBuffer 中的位置（如 [5] 表示位置 5 处有分隔符）
	confirmedSegments  []ConfirmedSegment // 拼音分步确认：已确认但未上屏的文本段
	candidates         []ui.Candidate
	currentPage        int
	totalPages         int
	candidatesPerPage  int
	selectedIndex      int  // 当前页内选中的候选索引（0-based），用于上下箭头键选择
	pendingFirstShow   bool // 首字符延迟显示：等待 C++ 响应后的准确位置再显示候选窗口

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

	// Composition start position: captured when inputBuffer transitions from empty to non-empty.
	// Used to anchor the candidate window at the start of the composition when inline preedit is enabled,
	// instead of following the current caret position which moves as the user types.
	compositionStartX     int
	compositionStartY     int
	compositionStartValid bool

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

	// Dark mode watcher for system theme changes
	darkModeWatcher *theme.DarkModeWatcher

	// 输入历史：追踪最近上屏文字，用于加词推荐
	inputHistory *InputHistory

	// 加词模式状态
	addWordActive bool   // 是否处于加词模式
	addWordChars  []rune // 可选字符池
	addWordLen    int    // 当前选取的词长
	addWordCode   string // 自动计算的编码
}

// BridgeServer interface for broadcasting state to TSF clients
type BridgeServer interface {
	PushStateToAllClients(status *bridge.StatusUpdateData)
	PushCommitTextToActiveClient(text string) // Only send to active client for security
	PushClearCompositionToActiveClient()      // Clear inline composition on active client
	RestartService()
	// GetActiveHostRender returns write/hide functions if the active process has host rendering.
	// Returns nil functions if host rendering is not active for the current process.
	GetActiveHostRender() (writeFrame func(img *image.RGBA, x, y int) error, hideFunc func())
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

// getIconLabelNoLock computes the taskbar icon label based on current state (caller must hold lock)
// This determines what character is displayed in the Windows taskbar input indicator
// Currently uses simple 中/英/A labels; engine-specific labels (拼/五/双) planned for future
func (c *Coordinator) getIconLabelNoLock() string {
	effectiveChinese := c.chineseMode && !c.capsLockOn
	if effectiveChinese {
		return "中"
	}
	if c.capsLockOn {
		return "A"
	}
	return "英"
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
		IconLabel:          c.getIconLabelNoLock(),
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
		ChinesePunct:  c.chinesePunctuation && effectiveMode == ModeChinese,
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
		inputHistory:       NewInputHistory(20),
	}

	// Set up toolbar callbacks
	c.setupToolbarCallbacks()

	// Set up candidate window callbacks for mouse interaction
	c.setupCandidateCallbacks()

	// Set up global hotkey callbacks (RegisterHotKey for combination hotkeys)
	c.setupGlobalHotkeyCallbacks()

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
		// 设置文本渲染模式
		if cfg.UI.TextRenderMode != "" {
			c.uiManager.SetTextRenderMode(cfg.UI.TextRenderMode)
		}
		// 设置候选框GDI字体参数
		if cfg.UI.GDIFontWeight > 0 || cfg.UI.GDIFontScale > 0 {
			c.uiManager.SetGDIFontParams(cfg.UI.GDIFontWeight, cfg.UI.GDIFontScale)
		}
		// 设置菜单GDI字体参数（独立于候选框）
		if cfg.UI.MenuFontWeight > 0 {
			c.uiManager.SetMenuFontParams(cfg.UI.MenuFontWeight, cfg.UI.GDIFontScale)
		}
		// 设置菜单字体大小
		if cfg.UI.MenuFontSize > 0 {
			c.uiManager.SetMenuFontSize(cfg.UI.MenuFontSize)
		}
		// 初始化主题暗色模式并加载主题
		c.initThemeMode(cfg)
	}

	return c
}

// initThemeMode initializes the dark mode state and starts the system theme watcher if needed
func (c *Coordinator) initThemeMode(cfg *config.Config) {
	if c.uiManager == nil {
		return
	}

	themeStyle := cfg.UI.ThemeStyle
	if themeStyle == "" {
		themeStyle = theme.ThemeStyleSystem
	}

	// Determine initial dark mode state
	isDark := false
	switch themeStyle {
	case theme.ThemeStyleDark:
		isDark = true
	case theme.ThemeStyleLight:
		isDark = false
	default: // system
		isDark = theme.IsSystemDarkMode()
	}

	// Set dark mode on the theme manager before loading the theme
	c.uiManager.SetDarkMode(isDark)

	// Load the theme
	themeName := cfg.UI.Theme
	if themeName == "" {
		themeName = "default"
	}
	c.uiManager.LoadTheme(themeName)

	// Start system theme watcher if following system mode
	if themeStyle == theme.ThemeStyleSystem {
		c.startDarkModeWatcher()
	}
}

// startDarkModeWatcher starts watching for system dark mode changes
func (c *Coordinator) startDarkModeWatcher() {
	// Stop existing watcher if any
	if c.darkModeWatcher != nil {
		c.darkModeWatcher.Stop()
	}

	c.darkModeWatcher = theme.NewDarkModeWatcher(c.logger, func(isDark bool) {
		// Called on system theme change — re-resolve and apply the theme
		if c.uiManager != nil {
			c.uiManager.SetDarkMode(isDark)
			c.uiManager.ReapplyTheme()
		}
	})
	c.darkModeWatcher.Start()
}

// stopDarkModeWatcher stops the system dark mode watcher
func (c *Coordinator) stopDarkModeWatcher() {
	if c.darkModeWatcher != nil {
		c.darkModeWatcher.Stop()
		c.darkModeWatcher = nil
	}
}

// hasPendingInput 检查是否有任何类型的待处理输入
func (c *Coordinator) hasPendingInput() bool {
	return len(c.inputBuffer) > 0 || len(c.confirmedSegments) > 0 || len(c.tempEnglishBuffer) > 0 || len(c.tempPinyinBuffer) > 0
}

// getPendingBufferText 获取当前待处理缓冲区的文本（用于 CommitOnSwitch 上屏）
// 优先级：主输入缓冲（含确认段）> 临时英文缓冲 > 临时拼音缓冲
func (c *Coordinator) getPendingBufferText() string {
	// 如果有确认段，拼接确认文本 + 剩余编码
	if len(c.confirmedSegments) > 0 || len(c.inputBuffer) > 0 {
		var text string
		for _, seg := range c.confirmedSegments {
			text += seg.Text
		}
		text += c.inputBuffer
		if c.fullWidth {
			return transform.ToFullWidth(text)
		}
		return text
	}

	var text string
	switch {
	case len(c.tempEnglishBuffer) > 0:
		text = c.tempEnglishBuffer
	case len(c.tempPinyinBuffer) > 0:
		text = c.tempPinyinBuffer
	default:
		return ""
	}
	if c.fullWidth {
		return transform.ToFullWidth(text)
	}
	return text
}

func (c *Coordinator) clearState() {
	c.inputBuffer = ""
	c.inputCursorPos = 0
	c.preeditDisplay = ""
	c.syllableBoundaries = nil
	c.confirmedSegments = nil
	c.tempEnglishMode = false
	c.tempEnglishBuffer = ""
	// 清除临时拼音状态时，同步卸载引擎层的拼音词库层，避免污染五笔查询
	if c.tempPinyinMode && c.engineMgr != nil {
		c.engineMgr.DeactivateTempPinyin()
	}
	c.tempPinyinMode = false
	c.tempPinyinBuffer = ""
	c.candidates = nil
	c.currentPage = 1
	c.totalPages = 1
	c.selectedIndex = 0
	c.pendingFirstShow = false
	c.compositionStartValid = false
	// 清理加词模式状态
	c.addWordActive = false
	c.addWordChars = nil
	c.addWordLen = 0
	c.addWordCode = ""

	// 清除命令结果缓存，确保 uuid/date/time 等下次生成新值
	c.engineMgr.InvalidateCommandCache()
}
