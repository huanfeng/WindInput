//go:build darwin

package ui

import (
	"image"
	"image/color"
	"log/slog"
	"strings"
	"sync"

	"github.com/huanfeng/wind_input/internal/uicmd"
	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/theme"
)

// manager_darwin.go 提供 ui.Manager 在 darwin 上的 stub 实现。
//
// 设计要点:
//   - 保留 cmdCh / eventCh 通道, 让所有 setter / show / hide 调用仍投递 uicmd.Command。
//     这样未来 PR-6/7 加入的 macOS forwarder 可从 cmdCh 抽取命令转发给 IMKit `.app`,
//     从 Events() 订阅用户交互事件。
//   - Win-only 渲染 / 窗口管理 / 钩子均为 no-op, 函数返回零值。
//   - 类型镜像不依赖 windows.Handle, 所有 stub method 自洽无 cgo。
//
// 凡是 darwin 上调用方真的会用到的 method, 都通过 cmdCh 走平台无关命令通道,
// 让"未来 forwarder 直接订阅 cmdCh"成为最干净的接入点 — 不需要 darwin 端自己做翻译。

// Manager 在 darwin 上的占位类型。字段最小化, 不包含任何 Win 渲染器/句柄,
// 仅保留命令/事件通道与 coordinator 端可能查询的状态字段。
type Manager struct {
	logger *slog.Logger

	mu    sync.Mutex
	ready bool

	cmdCh   chan uicmdItem
	eventCh chan uicmd.Event
	readyCh chan struct{}

	// 配置/状态镜像 (darwin forwarder 暂未消费, 但保留语义为日后扩展)
	hideCandidateWindow  bool
	hidePreedit          bool
	preeditMode          config.PreeditMode
	pagerBarDisplay      config.PagerBarDisplay
	pageNumberDisplay    config.PageNumberDisplay
	cmdbarPrefix         string
	maxCandidateChars    int
	candidateIndexLabels string
	isPinyinMode         bool
	isQuickInputMode     bool
	modeLabel            string
	modeAccentColor      color.Color
	appPinEnabled        bool
	appPinPositions      map[string][2]int
	candidateLayout      config.CandidateLayout
	statusIndicatorCfg   StatusWindowConfig
	tooltipDelay         int
	chaiziFontPath       string // 拆字字根字体文件绝对路径 (随 tooltip 下发给 .app 注册)

	// callback 引用 (darwin 上 forwarder 直接订阅 Events(), 这些回调暂留兼容)
	candidateCallbacks *CandidateCallback
	toolbarCallbacks   *ToolbarCallback
	hotkeyCallback     func(command string)
}

// NewManager 创建 darwin 占位 Manager。Start() 默认立刻 ready (无窗口创建步骤)。
func NewManager(logger *slog.Logger) *Manager {
	return &Manager{
		logger:  logger,
		cmdCh:   make(chan uicmdItem, 100),
		eventCh: make(chan uicmd.Event, 100),
		readyCh: make(chan struct{}),
	}
}

// Start 在 darwin 上立即标记 ready 并返回; 真正的 UI 由 IMKit `.app` 在另一进程处理。
func (m *Manager) Start() error {
	m.mu.Lock()
	m.ready = true
	m.mu.Unlock()
	close(m.readyCh)
	m.logger.Info("ui.Manager (darwin stub) ready; forwarder should subscribe cmdCh + Events()")
	return nil
}

// WaitReady / IsReady / Destroy 与 Win 版语义对齐。
func (m *Manager) WaitReady() { <-m.readyCh }
func (m *Manager) IsReady() bool {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.ready
}
func (m *Manager) Destroy() {}

// Events 返回反向事件通道, 与 Win 版一致。
func (m *Manager) Events() <-chan uicmd.Event { return m.eventCh }

// SubscribeCommands 启一个 goroutine 把 darwin Manager 内部 cmdCh 推给 handler。
// darwin forwarder 用此把命令转成 SHM bitmap + bridge push 帧。
// 同时透传旁路 candidates ([]Candidate, 含 Weight/Source/Index/flags 等完整字段),
// 供 forwarder 直接喂 ui.Renderer.RenderCandidates (CandidatesShowPayload.Candidates
// 是精简 wire 版, 字段不全)。命令无候选旁路时 candidates 为 nil。
// 多次调用会启多个消费者, 命令被任一消费者抢占, 一般只调一次。
func (m *Manager) SubscribeCommands(handler func(cmd uicmd.Command, candidates []Candidate)) {
	go func() {
		for item := range m.cmdCh {
			handler(item.Cmd, item.Candidates)
		}
	}()
}

// postCmd 投递一个 uicmd.Command (非阻塞)。
func (m *Manager) postCmd(cmd uicmd.Command) {
	select {
	case m.cmdCh <- uicmdItem{Cmd: cmd}:
	default:
		m.logger.Debug("darwin Manager cmdCh full, dropping", "type", cmd.Type.String())
	}
}

// ============================================================================
// 候选框
// ============================================================================

func (m *Manager) ShowCandidates(candidates []Candidate, input string,
	cursorPos, caretX, caretY, caretHeight, page, totalPages,
	totalCandidateCount, candidatesPerPage, selectedIndex int) error {
	m.mu.Lock()
	if !m.ready {
		m.mu.Unlock()
		return nil
	}
	m.mu.Unlock()
	item := uicmdItem{
		Cmd: uicmd.NewCommand(uicmd.CmdCandidatesShow, 0, uicmd.CandidatesShowPayload{
			Candidates:          toUICandidates(candidates),
			Input:               input,
			CursorPos:           cursorPos,
			CaretX:              caretX,
			CaretY:              caretY,
			CaretHeight:         caretHeight,
			Page:                page,
			TotalPages:          totalPages,
			TotalCandidateCount: totalCandidateCount,
			CandidatesPerPage:   candidatesPerPage,
			SelectedIndex:       selectedIndex,
		}),
		Candidates: candidates,
	}
	select {
	case m.cmdCh <- item:
	default:
	}
	return nil
}

func (m *Manager) Hide() {
	m.postCmd(uicmd.NewCommand(uicmd.CmdCandidatesHide, 0, uicmd.CandidatesHidePayload{}))
}

func (m *Manager) UpdatePosition(x, y int) {
	m.postCmd(uicmd.NewCommand(uicmd.CmdCandidatesPosition, 0,
		uicmd.CandidatesPositionPayload{X: x, Y: y}))
}

func (m *Manager) IsVisible() bool           { return false }
func (m *Manager) RefreshCandidates()        {}
func (m *Manager) NotifyDPIChanged()         {}
func (m *Manager) IsCandidateMenuOpen() bool { return false }
func (m *Manager) HideCandidateMenu() {
	m.postCmd(uicmd.NewCommand(uicmd.CmdCandidateMenuHide, 0, uicmd.CandidateMenuHidePayload{}))
}
func (m *Manager) CandidateMenuContainsPoint(int, int) bool { return false }

func (m *Manager) SetPinyinMode(b bool) {
	m.mu.Lock()
	m.isPinyinMode = b
	m.mu.Unlock()
	m.postCmd(m.snapshotMarkers())
}

func (m *Manager) SetQuickInputMode(b bool) {
	m.mu.Lock()
	m.isQuickInputMode = b
	m.mu.Unlock()
	m.postCmd(m.snapshotMarkers())
}

func (m *Manager) SetModeLabel(label string) {
	m.mu.Lock()
	m.modeLabel = label
	m.mu.Unlock()
	m.postCmd(m.snapshotMarkers())
}

func (m *Manager) SetModeAccentColor(c color.Color) {
	m.mu.Lock()
	m.modeAccentColor = c
	m.mu.Unlock()
	m.postCmd(m.snapshotMarkers())
}

// snapshotMarkers darwin 端的 CmdCandidatesMarkers 全量快照。
func (m *Manager) snapshotMarkers() uicmd.Command {
	m.mu.Lock()
	payload := uicmd.CandidatesMarkersPayload{
		IsPinyinMode:     m.isPinyinMode,
		IsQuickInputMode: m.isQuickInputMode,
		ModeLabel:        m.modeLabel,
		AccentColor:      toUIColorPtrDarwin(m.modeAccentColor),
	}
	m.mu.Unlock()
	return uicmd.NewCommand(uicmd.CmdCandidatesMarkers, 0, payload)
}

func toUIColorPtrDarwin(c color.Color) *uicmd.Color {
	if c == nil {
		return nil
	}
	r, g, b, a := c.RGBA()
	return &uicmd.Color{R: uint8(r >> 8), G: uint8(g >> 8), B: uint8(b >> 8), A: uint8(a >> 8)}
}

// ============================================================================
// 工具栏
// ============================================================================

func (m *Manager) SetToolbarVisible(visible bool) {
	if !visible {
		m.postCmd(uicmd.NewCommand(uicmd.CmdToolbarHide, 0, uicmd.ToolbarHidePayload{}))
	}
}
func (m *Manager) ShowToolbarWithState(x, y int, state ToolbarState) {
	m.postCmd(uicmd.NewCommand(uicmd.CmdToolbarShow, 0, uicmd.ToolbarShowPayload{
		X: x, Y: y, State: toUIToolbarStateD(state),
	}))
}
func (m *Manager) UpdateToolbarState(state ToolbarState) {
	m.postCmd(uicmd.NewCommand(uicmd.CmdToolbarUpdate, 0, uicmd.ToolbarUpdatePayload{
		State: toUIToolbarStateD(state),
	}))
}
func (m *Manager) SetToolbarPosition(int, int)    {}
func (m *Manager) GetToolbarPosition() (int, int) { return 0, 0 }
func (m *Manager) IsToolbarMenuOpen() bool        { return false }
func (m *Manager) HideToolbarMenu() {
	m.postCmd(uicmd.NewCommand(uicmd.CmdToolbarMenuHide, 0, uicmd.ToolbarMenuHidePayload{}))
}
func (m *Manager) ToolbarMenuContainsPoint(int, int) bool { return false }

func (m *Manager) ShowUnifiedMenu(screenX, screenY, flipRefY int, state UnifiedMenuState, callback func(id int)) {
	item := uicmdItem{
		Cmd: uicmd.NewCommand(uicmd.CmdMenuShow, 0, uicmd.MenuShowPayload{
			ScreenX: screenX, ScreenY: screenY, FlipRefY: flipRefY,
		}),
		MenuState: &state,
		Callback:  callback,
	}
	select {
	case m.cmdCh <- item:
	default:
	}
}
func (m *Manager) IsUnifiedMenuOpen() bool { return false }
func (m *Manager) HideUnifiedMenu()        {}

func toUIToolbarStateD(s ToolbarState) uicmd.ToolbarState {
	return uicmd.ToolbarState{
		ChineseMode:   s.ChineseMode,
		CapsLock:      s.CapsLock,
		FullWidth:     s.FullWidth,
		ChinesePunct:  s.ChinesePunct,
		EffectiveMode: int32(s.EffectiveMode),
		ModeLabel:     s.ModeLabel,
	}
}

// ============================================================================
// 状态指示器 / 模式浮窗
// ============================================================================

func (m *Manager) ShowModeIndicator(mode string, x, y int) {
	m.postCmd(uicmd.NewCommand(uicmd.CmdModeShow, 0,
		uicmd.ModeShowPayload{Mode: mode, X: x, Y: y}))
}
func (m *Manager) ShowStatusIndicator(state StatusState, x, y, height int) {
	m.postCmd(uicmd.NewCommand(uicmd.CmdStatusShow, 0, uicmd.StatusShowPayload{
		State:  uicmd.StatusState{ModeLabel: state.ModeLabel, PunctLabel: state.PunctLabel, WidthLabel: state.WidthLabel},
		X:      x,
		Y:      y,
		Height: height,
	}))
}
func (m *Manager) HideStatusIndicator() {
	m.postCmd(uicmd.NewCommand(uicmd.CmdStatusHide, 0, uicmd.StatusHidePayload{}))
}
func (m *Manager) GetStatusWindow() *StatusWindow { return nil }

// ============================================================================
// Tooltip
// ============================================================================

func (m *Manager) HideTooltip() {
	m.postCmd(uicmd.NewCommand(uicmd.CmdTooltipHide, 0, uicmd.TooltipHidePayload{}))
}
func (m *Manager) ShowTooltipText(text string, centerX, belowY, aboveY int) {
	if text == "" {
		return
	}
	m.mu.Lock()
	fontPath := m.chaiziFontPath
	m.mu.Unlock()
	m.postCmd(uicmd.NewCommand(uicmd.CmdTooltipShow, 0, uicmd.TooltipShowPayload{
		Text: text, CenterX: centerX, BelowY: belowY, AboveY: aboveY, FontPath: fontPath,
	}))
}

// SetTooltipChaiziFont 记录拆字字根字体文件路径, 随后续 tooltip 一并下发给 .app
// 注册并级联回退渲染 PUA 字根。dwFamilyName (Windows DirectWrite 系统字体名) 在
// macOS 不适用, 忽略。
func (m *Manager) SetTooltipChaiziFont(fontPath, _ string) {
	m.mu.Lock()
	m.chaiziFontPath = fontPath
	m.mu.Unlock()
}
func (m *Manager) SetTooltipDelay(delay int) {
	m.mu.Lock()
	m.tooltipDelay = delay
	m.mu.Unlock()
}

// ============================================================================
// Toast
// ============================================================================

func (m *Manager) ShowToast(opts ToastOptions) {
	m.postCmd(uicmd.NewCommand(uicmd.CmdToastShow, 0, uicmd.ToastShowPayload{
		Title: opts.Title, Message: opts.Message,
		Level:    toastLevelToWire(opts.Level),
		Position: toastPositionToWire(opts.Position),
		Duration: int32(opts.Duration), MaxWidth: int32(opts.MaxWidth),
	}))
}
func (m *Manager) HideToast() {
	m.postCmd(uicmd.NewCommand(uicmd.CmdToastHide, 0, uicmd.ToastHidePayload{}))
}
func (m *Manager) ShowToastError(title, message string) {
	m.ShowToast(ToastOptions{Title: title, Message: message, Level: ToastError, Position: ToastCenter, Duration: 5000})
}
func (m *Manager) ShowToastSuccess(message string) {
	m.ShowToast(ToastOptions{Message: message, Level: ToastSuccess, Position: ToastBottomRight, Duration: 3500})
}

// ============================================================================
// 按键合成 (命令直通车 key.* / clip.paste; 仅 darwin)
// ============================================================================
//
// 这些方法把已解析的按键组合 / 文本经 uicmd 命令投递给 forwarder, 由 forwarder
// 编 push 帧下发给 IMKit `.app`, 再由 `.app` 用 CGEvent (tap/seq/hold/release)
// 或 client.insertText (type) 实际执行。Go 服务进程无 GUI 事件上下文, 不直接合成。
// 由 coordinator/cmdbar_inject_darwin.go 在 cmdbar 动作求值时调用。

// SendKeyTap 下发单次按键组合。
func (m *Manager) SendKeyTap(key string, modifiers []string) {
	m.postCmd(uicmd.NewCommand(uicmd.CmdKeyTap, 0, uicmd.KeyTapPayload{Key: key, Modifiers: modifiers}))
}

// SendKeySeq 下发顺序多个按键组合。
func (m *Manager) SendKeySeq(combos []uicmd.KeyCombo) {
	if len(combos) == 0 {
		return
	}
	m.postCmd(uicmd.NewCommand(uicmd.CmdKeySeq, 0, uicmd.KeySeqPayload{Combos: combos}))
}

// SendKeyHold 下发按下并保持。
func (m *Manager) SendKeyHold(key string, modifiers []string) {
	m.postCmd(uicmd.NewCommand(uicmd.CmdKeyHold, 0, uicmd.KeyHoldPayload{Key: key, Modifiers: modifiers}))
}

// SendKeyRelease 下发抬起之前 hold 的组合。
func (m *Manager) SendKeyRelease(key string, modifiers []string) {
	m.postCmd(uicmd.NewCommand(uicmd.CmdKeyRelease, 0, uicmd.KeyReleasePayload{Key: key, Modifiers: modifiers}))
}

// SendKeyType 下发 Unicode 文本上屏 (走 .app client.insertText)。
func (m *Manager) SendKeyType(text string) {
	if text == "" {
		return
	}
	m.postCmd(uicmd.NewCommand(uicmd.CmdKeyType, 0, uicmd.KeyTypePayload{Text: text}))
}

func toastLevelToWire(l ToastLevel) uicmd.ToastLevel {
	switch l {
	case ToastSuccess:
		return uicmd.ToastSuccess
	case ToastWarn:
		return uicmd.ToastWarn
	case ToastError:
		return uicmd.ToastError
	default:
		return uicmd.ToastInfo
	}
}

func toastPositionToWire(p ToastPosition) uicmd.ToastPosition {
	switch p {
	case ToastBottomRight:
		return uicmd.ToastBottomRight
	default:
		return uicmd.ToastCenter
	}
}

// ============================================================================
// 主题
// ============================================================================

func (m *Manager) LoadTheme(string) error             { return nil }
func (m *Manager) ReapplyTheme()                      {}
func (m *Manager) SetDarkMode(bool)                   {}
func (m *Manager) GetAvailableThemes() []string       { return nil }
func (m *Manager) GetCurrentThemeName() string        { return "" }
func (m *Manager) ConsumeThemeFallbackNotice() string { return "" }

// GetCurrentThemeID 读 config 的 ui.theme (统一菜单主题项勾选用)。
func (m *Manager) GetCurrentThemeID() string {
	if cfg, err := config.Load(); err == nil {
		return cfg.UI.Theme
	}
	return ""
}

// GetAvailableThemeInfos 经 theme.Manager 列出可用主题 (服务进程内 exeDir/data/themes 可解析)。
func (m *Manager) GetAvailableThemeInfos() []theme.ThemeDisplayInfo {
	return theme.NewManager(m.logger).ListAvailableThemeInfos()
}

// ============================================================================
// 配置 setter
// ============================================================================

// UpdateConfig 与 Win 版同签名 (4 参)。darwin 上候选窗字体由 forwarder 自持的
// ui.Renderer 管理: 字号/字号跟随主题经 forwarder 读 config.yaml 生效 (applyFontFromConfig),
// 字体族恒用 forwarder 启动解析的本机 CJK 族 (config.UI.FontFamily 可能是 Win 字体名)。
// 故此 stub 仅镜像 hideCandidateWindow, fontSize/fontFollowTheme/fontFamily 不在此消费。
func (m *Manager) UpdateConfig(fontSize float64, fontFollowTheme bool, fontFamily string, hideCandidateWindow bool) {
	m.mu.Lock()
	m.hideCandidateWindow = hideCandidateWindow
	m.mu.Unlock()
}

// SetCandidateIndexLabels 与 Win 版同签名。darwin 上仅镜像到字段;实际生效路径是
// forwarder 经 config.yaml mtime 读取 CandidateIndexLabels 并 SetGlobalIndexLabels
// (见 cmd/service/forwarder_darwin.go applyFontFromConfig), 不走此 stub。
func (m *Manager) SetCandidateIndexLabels(labels string) {
	m.mu.Lock()
	m.candidateIndexLabels = labels
	m.mu.Unlock()
}

// SetBehaviorOverrides 与 Win 版同签名。darwin 上候选窗由 forwarder 自持的 ui.Renderer 管理，
// behavior 覆盖经 forwarder 读 config.yaml 生效，故此 stub 不消费。
func (m *Manager) SetBehaviorOverrides(
	alwaysShowPager, alwaysShowPagerFollowTheme bool,
	showPageNumber, showPageNumberFollowTheme bool,
	verticalMaxWidth int, verticalMaxWidthFollowTheme bool,
) {
}

func (m *Manager) UpdateStatusIndicatorConfig(duration, offsetX, offsetY int) {}
func (m *Manager) UpdateStatusIndicatorFullConfig(cfg StatusWindowConfig) {
	m.mu.Lock()
	m.statusIndicatorCfg = cfg
	m.mu.Unlock()
	m.postCmd(uicmd.NewCommand(uicmd.CmdStatusConfig, 0, uicmd.StatusConfigPayload{
		Enabled: cfg.Enabled, DisplayMode: uicmd.StatusDisplayMode(cfg.DisplayMode),
		Duration: int32(cfg.Duration), SchemaNameStyle: cfg.SchemaNameStyle,
		ShowMode: cfg.ShowMode, ShowPunct: cfg.ShowPunct, ShowFullWidth: cfg.ShowFullWidth,
		PositionMode: uicmd.StatusPositionMode(cfg.PositionMode),
		OffsetX:      int32(cfg.OffsetX), OffsetY: int32(cfg.OffsetY),
		CustomX: int32(cfg.CustomX), CustomY: int32(cfg.CustomY),
		FontSize: cfg.FontSize, Opacity: cfg.Opacity,
		BackgroundColor: cfg.BackgroundColor, TextColor: cfg.TextColor,
		BorderRadius: cfg.BorderRadius,
	}))
}
func (m *Manager) SetCandidateLayout(layout config.CandidateLayout) {
	m.mu.Lock()
	m.candidateLayout = layout
	m.mu.Unlock()
	m.postCmd(m.snapshotConfig())
}
func (m *Manager) GetCandidateLayout() config.CandidateLayout {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.candidateLayout
}
func (m *Manager) SetHideCandidateWindow(hide bool) {
	m.mu.Lock()
	m.hideCandidateWindow = hide
	m.mu.Unlock()
	m.postCmd(m.snapshotConfig())
}
func (m *Manager) IsHideCandidateWindow() bool {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.hideCandidateWindow
}
func (m *Manager) SetGDIFontParams(int, float64)       {}
func (m *Manager) SetMenuFontParams(int, float64)      {}
func (m *Manager) SetMenuFontSize(float64)             {}
func (m *Manager) SetTextRenderMode(config.FontEngine) {}
func (m *Manager) SetFlipLayoutWhenAbove(_ bool)       {}
func (m *Manager) SetHidePreedit(hide bool) {
	m.mu.Lock()
	m.hidePreedit = hide
	m.mu.Unlock()
	m.postCmd(m.snapshotConfig())
}
func (m *Manager) SetPreeditMode(mode config.PreeditMode) {
	m.mu.Lock()
	m.preeditMode = mode
	m.mu.Unlock()
	m.postCmd(m.snapshotConfig())
}
func (m *Manager) SetPagerBarDisplay(mode config.PagerBarDisplay) {
	m.mu.Lock()
	m.pagerBarDisplay = mode
	m.mu.Unlock()
	m.postCmd(m.snapshotConfig())
}
func (m *Manager) SetPageNumberDisplay(mode config.PageNumberDisplay) {
	m.mu.Lock()
	m.pageNumberDisplay = mode
	m.mu.Unlock()
	m.postCmd(m.snapshotConfig())
}
func (m *Manager) SetCmdbarCandidatePrefix(prefix string) {
	m.mu.Lock()
	m.cmdbarPrefix = prefix
	m.mu.Unlock()
	m.postCmd(m.snapshotConfig())
}
func (m *Manager) GetCmdbarCandidatePrefix() string {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.cmdbarPrefix
}
func (m *Manager) SetMaxCandidateChars(n int) {
	m.mu.Lock()
	m.maxCandidateChars = n
	m.mu.Unlock()
	m.postCmd(m.snapshotConfig())
}
func (m *Manager) snapshotConfig() uicmd.Command {
	m.mu.Lock()
	p := uicmd.CandidatesConfigPayload{
		Layout:              uicmd.CandidateLayout(m.candidateLayout),
		HideCandidateWindow: m.hideCandidateWindow,
		HidePreedit:         m.hidePreedit,
		PreeditMode:         uicmd.PreeditMode(m.preeditMode),
		PagerBarDisplay:     uicmd.PagerBarDisplay(m.pagerBarDisplay),
		PageNumberDisplay:   uicmd.PageNumberDisplay(m.pageNumberDisplay),
		CmdbarPrefix:        m.cmdbarPrefix,
		MaxCandidateChars:   m.maxCandidateChars,
	}
	m.mu.Unlock()
	return uicmd.NewCommand(uicmd.CmdCandidatesConfig, 0, p)
}

func (m *Manager) SetActiveAppPinState(enabled bool, positionsByMonitor map[string][2]int) {
	m.mu.Lock()
	m.appPinEnabled = enabled
	if enabled {
		m.appPinPositions = positionsByMonitor
	} else {
		m.appPinPositions = nil
	}
	m.mu.Unlock()
	m.postCmd(uicmd.NewCommand(uicmd.CmdCandidatesPinState, 0, uicmd.CandidatesPinStatePayload{
		Enabled: enabled, PositionsByMonitor: positionsByMonitor,
	}))
}

// ============================================================================
// 设置 / 启动外部进程
// ============================================================================

func (m *Manager) OpenSettings() { m.OpenSettingsWithPage("") }
func (m *Manager) OpenSettingsWithPage(page string) {
	m.postCmd(uicmd.NewCommand(uicmd.CmdSettingsOpen, 0, uicmd.SettingsOpenPayload{Page: page}))
}

// ============================================================================
// 快捷键
// ============================================================================

func (m *Manager) SetGlobalHotkeyCallback(cb func(command string)) {
	m.hotkeyCallback = cb
}
func (m *Manager) RegisterGlobalHotkeys(entries []GlobalHotkeyEntry) {
	wire := make([]uicmd.HotkeyEntry, len(entries))
	for i, e := range entries {
		wire[i] = uicmd.HotkeyEntry{
			ID: int32(e.ID), Mods: e.Modifiers, KeyCode: e.VK, Command: e.Command,
		}
	}
	m.postCmd(uicmd.NewCommand(uicmd.CmdHotkeysRegister, 0, uicmd.HotkeysRegisterPayload{Entries: wire}))
}
func (m *Manager) UnregisterGlobalHotkeys() {
	m.postCmd(uicmd.NewCommand(uicmd.CmdHotkeysUnregister, 0, uicmd.HotkeysUnregisterPayload{}))
}

// TakeUIScreenshots 在 darwin 为 no-op: 候选/状态等窗口由 IMKit .app 渲染, Go 端无可截窗口。
func (m *Manager) TakeUIScreenshots() {}

// ============================================================================
// Host render (Win 专有, darwin 无概念)
// ============================================================================

func (m *Manager) SetHostRenderFunc(func(img *image.RGBA, x, y int) error, func()) {}
func (m *Manager) IsHostRendering() bool                                           { return false }

// ============================================================================
// callback 注册
// ============================================================================

func (m *Manager) SetToolbarCallbacks(cb *ToolbarCallback) {
	m.mu.Lock()
	m.toolbarCallbacks = cb
	m.mu.Unlock()
}
func (m *Manager) SetCandidateCallbacks(cb *CandidateCallback) {
	m.mu.Lock()
	m.candidateCallbacks = cb
	m.mu.Unlock()
}

// ============================================================================
// 独立函数 stub
// ============================================================================

// StatusWindow 占位类型 (darwin 不实例化此结构, GetStatusWindow 返回 nil)。
// 保留几个 coordinator 调用面要求的方法 stub, 防止 nil pointer 时 darwin 编译失败。
type StatusWindow struct{}

// CancelPendingHide darwin 上空实现 (没有 host render hide 定时器)。
func (w *StatusWindow) CancelPendingHide() {}

// SetMenuCallback darwin 上空实现 (状态指示器菜单由 IMKit `.app` 内自绘 NSMenu)。
func (w *StatusWindow) SetMenuCallback(cb func(action StatusMenuAction)) {}

func GetCapsLockState() bool { return false }
func GetSystemDPI() int      { return 96 }
func SetEffectiveDPI(int)    {}

// ScaleIntForDPI / ScaleForDPI / GetDPIScale 由跨平台 dpi_neutral.go 提供
// (darwin 默认 dpiScaleProvider 返回 1.0)。
func GetMonitorWorkAreaFromPoint(int, int) (left, top, right, bottom int) {
	return 0, 0, 1920, 1080
}
func MonitorKeyStr(workRight, workBottom int) string {
	return "darwin:" + intToA(workRight) + "," + intToA(workBottom)
}
func GetDefaultToolbarPosition(int, int) (int, int)            { return 0, 0 }
func GetToolbarPositionForCaret(int, int, int, int) (int, int) { return 0, 0 }

// ParseHotkeyString 在 darwin 上提供与 Win 同名的解析实现 (基础语义), 让 coordinator
// 代码可以编译; 实际快捷键注册由 IMKit `.app` 拦截 CGEventTap 完成。
func ParseHotkeyString(s string, id int, command string) (GlobalHotkeyEntry, bool) {
	if s == "" || strings.EqualFold(s, "none") {
		return GlobalHotkeyEntry{}, false
	}
	// 简化版: 仅返回带 command 的占位 entry, 修饰键/键码由未来 macOS 端解析。
	return GlobalHotkeyEntry{ID: id, Command: command}, true
}

func intToA(v int) string {
	// 避免在 stub 里 import strconv 的痕量考虑; 用 fmt.Sprintf 也行但 strconv 更直接
	// 这里手写以保持依赖最少
	if v == 0 {
		return "0"
	}
	neg := v < 0
	if neg {
		v = -v
	}
	var buf [20]byte
	i := len(buf)
	for v > 0 {
		i--
		buf[i] = byte('0' + v%10)
		v /= 10
	}
	if neg {
		i--
		buf[i] = '-'
	}
	return string(buf[i:])
}
