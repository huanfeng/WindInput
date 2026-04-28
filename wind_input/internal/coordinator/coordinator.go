// Package coordinator orchestrates communication between C++ Bridge, Engine, and UI
package coordinator

import (
	"image"
	"log/slog"
	"sync"
	"time"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/candidate"
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

// caretState 光标位置与自适应检测相关状态
type caretState struct {
	// 当前光标位置（来自 C++ TSF Bridge）
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

	// 当前活跃进程信息
	activeProcessID   uint32    // 当前活跃进程 ID
	activeProcessName string    // 当前活跃进程名（小写）
	lastKeyTime       time.Time // 最近一次按键处理的时间

	// 首次 show 推迟：当本次按键启动一个新的 composition 时（如 WPS 会触发文本
	// reflow 让光标位置变化），不能用按键前的旧坐标显示候选窗，否则会先在错误
	// 位置出现再跳到正确位置。设置 pendingFirstShow=true 后，handleAlphaKey 不
	// 立即 showUI；待 HandleCaretUpdate 收到 reflow 后的新坐标时再 show。
	// pendingFirstShowToken 用于超时回调比对身份。
	pendingFirstShow      bool
	pendingFirstShowToken uint64
}

// tempModeState 临时输入模式（临时英文/临时拼音）状态
type tempModeState struct {
	tempEnglishMode       bool                  // 是否处于临时英文模式
	tempEnglishTriggerKey string                // 当前使用的触发键类型（触发键方式进入时）
	tempEnglishBuffer     string                // 临时英文缓冲区
	tempEnglishCursorPos  int                   // 临时英文光标位置
	tempEnglishCandidates []candidate.Candidate // 临时英文模式的英文候选列表
	tempPinyinMode        bool                  // 是否处于临时拼音模式
	tempPinyinBuffer      string                // 临时拼音输入缓冲区
	tempPinyinCursorPos   int                   // 临时拼音光标位置
	tempPinyinCommitted   string                // 临时拼音部分上屏累积文本
	tempPinyinTriggerKey  string                // 临时拼音触发键类型（"backtick"/"semicolon"/"z"）
}

// addWordState 快捷加词模式状态
type addWordState struct {
	addWordActive bool   // 是否处于加词模式
	addWordChars  []rune // 可选字符池
	addWordLen    int    // 当前选取的词长
	addWordCode   string // 自动计算的编码
}

// quickInputState 快捷输入模式状态
type quickInputState struct {
	quickInputMode              bool   // 是否处于快捷输入模式
	quickInputTriggerKey        string // 当前使用的触发键类型（如 "semicolon"）
	quickInputBuffer            string // 触发键后的输入缓冲区（不含触发键本身）
	quickInputPinyinMode        bool   // 是否处于快捷输入的临时拼音子模式
	quickInputPinyinBuffer      string // 快捷输入临时拼音缓冲区
	quickInputPinyinCursorPos   int    // 快捷输入拼音光标位置
	quickInputPinyinCommitted   string // 快捷输入拼音部分上屏累积文本
	quickInputPinyinDictSwapped bool   // 是否已交换词库层（仅码表引擎下为 true）
	savedLayout                 string // 进入快捷输入前的布局（用于退出时恢复）
}

// Coordinator orchestrates between C++ Bridge, Engine, and native UI
type Coordinator struct {
	engineMgr    *engine.Manager
	uiManager    *ui.Manager
	logger       *slog.Logger
	config       *config.Config
	bridgeServer BridgeServer // Interface for broadcasting state to TSF clients
	version      string       // App version for display in menu

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
	preeditDisplay     string             // 带音节分隔符的显示文本（如 "zhong guo"），五笔时为空
	syllableBoundaries []int              // 音节边界在 inputBuffer 中的位置（如 [5] 表示位置 5 处有分隔符）
	confirmedSegments  []ConfirmedSegment // 拼音分步确认：已确认但未上屏的文本段
	candidates         []ui.Candidate
	currentPage        int
	totalPages         int
	candidatesPerPage  int
	selectedIndex      int // 当前页内选中的候选索引（0-based），用于上下箭头键选择

	// 分级加载状态
	candidateLimit    int    // 当前加载上限（0=无限制）
	candidateInput    string // 加载时的 inputBuffer 快照
	hasMoreCandidates bool   // 是否还有更多候选未加载

	// 光标位置与自适应检测
	caretState

	// 应用兼容性规则
	appCompat        *config.AppCompat     // 兼容性规则（从 compat.yaml 加载）
	activeCompatRule *config.AppCompatRule // 当前进程匹配的兼容性规则（nil 表示无特殊处理）

	// 临时输入模式
	tempModeState

	// Punctuation converter with state (for paired punctuation like quotes)
	punctConverter *transform.PunctuationConverter

	// Auto-pair tracker for bracket pairing (push on insert, pop on skip/delete)
	pairTracker    *transform.PairTracker
	pairTrackerEn  *transform.PairTracker // 英文配对追踪器
	pairInsertTime time.Time              // 最近一次自动配对插入的时间，用于抑制 SelectionChanged 清栈

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

	// 数字后智能标点：追踪上一个直通输出是否为数字
	// 用于在中文标点模式下将数字后的 。→. ，→, 自动转换为英文标点
	lastOutputWasDigit bool

	// 快捷加词模式
	addWordState

	// 快捷输入模式
	quickInputState
}

// BridgeServer interface for broadcasting state to TSF clients
type BridgeServer interface {
	PushStateToAllClients(status *bridge.StatusUpdateData)
	PushCommitTextToActiveClient(text string)                      // Only send to active client for security
	PushClearCompositionToActiveClient()                           // Clear inline composition on active client
	PushUpdateCompositionToActiveClient(text string, caretPos int) // Update inline composition on active client (mouse partial confirm)
	PushEnglishPairConfigToAllClients(enabled bool, pairs []string)
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

// SetVersion sets the app version for display in the menu
func (c *Coordinator) SetVersion(v string) {
	c.version = v
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

// isEffectiveChinesePunct 返回当前是否应使用中文标点（考虑 CapsLock 等模式影响）
// CapsLock 开启时视为英文模式，不使用中文标点。调用者必须持有锁。
func (c *Coordinator) isEffectiveChinesePunct() bool {
	return c.chinesePunctuation && c.getEffectiveModeNoLock() == ModeChinese
}

// IsCapsLockOn returns the current CapsLock state
func (c *Coordinator) IsCapsLockOn() bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.capsLockOn
}

// getIconLabelNoLock computes the taskbar icon label based on current state (caller must hold lock)
// This determines what character is displayed in the Windows taskbar input indicator
// Chinese mode uses the schema's icon_label (e.g., "拼", "五", "双", "混")
func (c *Coordinator) getIconLabelNoLock() string {
	effectiveChinese := c.chineseMode && !c.capsLockOn
	if effectiveChinese {
		if c.engineMgr != nil {
			_, iconLabel := c.engineMgr.GetSchemaDisplayInfo()
			if iconLabel != "" {
				return iconLabel
			}
		}
		return "中"
	}
	if c.capsLockOn {
		return "A"
	}
	return "英"
}

// BuildCurrentStatus returns the current status for external callers (e.g., initial state push).
func (c *Coordinator) BuildCurrentStatus() *bridge.StatusUpdateData {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.buildStatusUpdate()
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

// buildToolbarState creates a ToolbarState from current coordinator state (caller must hold lock)
func (c *Coordinator) buildToolbarState() ui.ToolbarState {
	effectiveMode := c.getEffectiveModeNoLock()

	// Get icon_label from current schema for toolbar display
	var modeLabel string
	if c.engineMgr != nil {
		_, modeLabel = c.engineMgr.GetSchemaDisplayInfo()
	}

	return ui.ToolbarState{
		ChineseMode:   effectiveMode == ModeChinese,
		FullWidth:     c.fullWidth,
		ChinesePunct:  c.isEffectiveChinesePunct(),
		CapsLock:      c.capsLockOn,
		EffectiveMode: int(effectiveMode),
		ModeLabel:     modeLabel,
	}
}

// syncToolbarStateNoLock synchronizes the current state to the toolbar (without lock)
func (c *Coordinator) syncToolbarStateNoLock() {
	if c.uiManager == nil {
		return
	}
	c.uiManager.UpdateToolbarState(c.buildToolbarState())
}

// NewCoordinator creates a new Coordinator
func NewCoordinator(engineMgr *engine.Manager, uiManager *ui.Manager, cfg *config.Config, appCompat *config.AppCompat, logger *slog.Logger) *Coordinator {
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
		caretState: caretState{
			caretX:      100,
			caretY:      100,
			caretHeight: 20,
		},
		punctConverter: transform.NewPunctuationConverter(),
		pairTracker:    transform.NewPairTracker(cfg.Input.AutoPair.ChinesePairs),
		pairTrackerEn:  transform.NewPairTracker(cfg.Input.AutoPair.EnglishPairs),
		hotkeyCompiler: hotkey.NewCompiler(cfg),
		hotkeysDirty:   true, // 首次使用时需要编译
		inputHistory:   NewInputHistory(20),
		appCompat:      appCompat,
	}

	// 根据配对表设置引号配对状态
	c.updatePairedQuotes(cfg.Input.AutoPair.ChinesePairs)

	// 加载自定义标点映射
	c.punctConverter.SetCustomMappings(cfg.Input.PunctCustom.Enabled, cfg.Input.PunctCustom.Mappings)

	// Set up toolbar callbacks
	c.setupToolbarCallbacks()

	// Set up candidate window callbacks for mouse interaction
	c.setupCandidateCallbacks()

	// 设置状态窗口右键菜单回调
	c.setupStatusWindowCallbacks()

	// Set up global hotkey callbacks (RegisterHotKey for combination hotkeys)
	c.setupGlobalHotkeyCallbacks()

	// Initialize UI config (including debug options)
	if c.uiManager != nil && cfg != nil {
		fontSpec := cfg.UI.FontFamily
		if fontSpec == "" {
			fontSpec = cfg.UI.FontPath
		}
		c.uiManager.UpdateConfig(cfg.UI.FontSize, fontSpec, cfg.UI.HideCandidateWindow)
		// Set candidate layout (horizontal/vertical)
		if cfg.UI.CandidateLayout != "" {
			c.uiManager.SetCandidateLayout(cfg.UI.CandidateLayout)
		}
		// Set hide preedit when inline preedit is enabled
		c.uiManager.SetHidePreedit(cfg.UI.InlinePreedit)
		// Set preedit display mode
		c.uiManager.SetPreeditMode(cfg.UI.PreeditMode)
		// Set status indicator config (旧字段兼容)
		c.uiManager.UpdateStatusIndicatorConfig(
			cfg.UI.StatusIndicatorDuration,
			cfg.UI.StatusIndicatorOffsetX,
			cfg.UI.StatusIndicatorOffsetY,
		)
		// 初始化完整状态提示配置
		siCfg := cfg.UI.StatusIndicator
		c.uiManager.UpdateStatusIndicatorFullConfig(ui.StatusWindowConfig{
			Enabled:         siCfg.Enabled,
			DisplayMode:     ui.StatusDisplayMode(siCfg.DisplayMode),
			Duration:        siCfg.Duration,
			SchemaNameStyle: siCfg.SchemaNameStyle,
			ShowMode:        siCfg.ShowMode,
			ShowPunct:       siCfg.ShowPunct,
			ShowFullWidth:   siCfg.ShowFullWidth,
			PositionMode:    ui.StatusPositionMode(siCfg.PositionMode),
			OffsetX:         siCfg.OffsetX,
			OffsetY:         siCfg.OffsetY,
			CustomX:         siCfg.CustomX,
			CustomY:         siCfg.CustomY,
			FontSize:        siCfg.FontSize,
			Opacity:         siCfg.Opacity,
			BackgroundColor: siCfg.BackgroundColor,
			TextColor:       siCfg.TextColor,
			BorderRadius:    siCfg.BorderRadius,
		})
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
	return len(c.inputBuffer) > 0 || len(c.confirmedSegments) > 0 || len(c.tempEnglishBuffer) > 0 || len(c.tempPinyinBuffer) > 0 || c.quickInputMode
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
	case c.quickInputMode && len(c.quickInputPinyinBuffer) > 0:
		text = c.quickInputPinyinBuffer
	case c.quickInputMode && len(c.quickInputBuffer) > 0:
		text = c.quickInputBuffer
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
	c.tempPinyinCommitted = ""
	c.candidates = nil
	c.currentPage = 1
	c.totalPages = 1
	c.selectedIndex = 0
	c.compositionStartValid = false
	// 清理加词模式状态
	c.addWordActive = false
	c.addWordChars = nil
	c.addWordLen = 0
	c.addWordCode = ""
	// 清理快捷输入模式状态（恢复布局需在重置标志前执行）
	if c.quickInputMode {
		// 如果处于快捷输入的临时拼音子模式且已交换词库层，先恢复
		if c.quickInputPinyinDictSwapped && c.engineMgr != nil {
			c.engineMgr.DeactivateTempPinyin()
		}
		if c.savedLayout != "" && c.uiManager != nil {
			c.uiManager.SetCandidateLayout(c.savedLayout)
		}
		if c.uiManager != nil {
			c.uiManager.SetQuickInputMode(false)
		}
	}
	if c.uiManager != nil {
		c.uiManager.SetModeLabel("")
	}
	c.quickInputMode = false
	c.quickInputBuffer = ""
	c.quickInputPinyinMode = false
	c.quickInputPinyinBuffer = ""
	c.quickInputPinyinCommitted = ""
	c.quickInputPinyinDictSwapped = false
	c.savedLayout = ""

	// 注意：不清除 activeProcessID，需要跨 composition 持久化

	// 清空配对栈（输入状态重置意味着光标位置不再可预测）
	if c.pairTracker != nil {
		c.pairTracker.Clear()
	}
	if c.pairTrackerEn != nil {
		c.pairTrackerEn.Clear()
	}

	// 清除命令结果缓存，确保 uuid/date/time 等下次生成新值
	c.engineMgr.InvalidateCommandCache()
}

// resetCompositionAnchorAfterCommit 在"部分上屏但保留剩余编码"等场景下被调用：
// 上屏会让 C++ 端结束当前 composition 并立即开启新 composition，旧的 compositionStart
// 锁定不应继续生效，否则候选窗会停留在前一段 composition 的位置。
// C++ 端会在新 composition 创建时通过 _compositionJustStarted 推迟首次 IPC 至
// OnLayoutChange，因此 Go 端无需再维护"等待坐标"的状态。
// 调用方必须持有 c.mu 锁。
func (c *Coordinator) resetCompositionAnchorAfterCommit() {
	c.compositionStartValid = false
}
