package ui

import (
	"os"
	"path/filepath"
	"strings"
	"unsafe"

	"golang.org/x/sys/windows"

	"github.com/huanfeng/wind_input/pkg/buildvariant"
	"github.com/huanfeng/wind_input/pkg/config"
)

// settingsLaunchAttempt 记录单次 wind_setting.exe 启动尝试的结果，仅用于日志聚合。
type settingsLaunchAttempt struct {
	path   string
	ret    uintptr
	err    error
	exists bool
}

// UpdateConfig 更新 UI 配置（热更新）
// fontFamily 仅作用于候选窗口渲染器，菜单/工具栏/提示等组件使用系统默认字体。
func (m *Manager) UpdateConfig(fontSize float64, fontFamily string, hideCandidateWindow bool) {
	// 候选字体仅影响候选窗口渲染器
	if m.renderer != nil {
		m.renderer.UpdateFont(fontSize, fontFamily)
	}
	// 更新调试开关
	m.mu.Lock()
	m.hideCandidateWindow = hideCandidateWindow
	m.mu.Unlock()
	m.logger.Info("UI config updated", "fontSize", fontSize, "fontFamily", fontFamily, "hideCandidateWindow", hideCandidateWindow)
}

// UpdateStatusIndicatorConfig 更新状态提示配置
func (m *Manager) UpdateStatusIndicatorConfig(duration, offsetX, offsetY int) {
	m.mu.Lock()
	if duration > 0 {
		m.statusIndicatorDuration = duration
	}
	m.statusIndicatorOffsetX = offsetX
	m.statusIndicatorOffsetY = offsetY
	m.mu.Unlock()
	m.logger.Info("Status indicator config updated", "duration", duration, "offsetX", offsetX, "offsetY", offsetY)
}

// UpdateStatusIndicatorFullConfig 更新完整状态提示配置
func (m *Manager) UpdateStatusIndicatorFullConfig(cfg StatusWindowConfig) {
	if m.status != nil {
		m.status.SetConfig(cfg)
	}
	// 同步旧字段保持兼容
	m.mu.Lock()
	m.statusIndicatorDuration = cfg.Duration
	m.statusIndicatorOffsetX = cfg.OffsetX
	m.statusIndicatorOffsetY = cfg.OffsetY
	m.mu.Unlock()
	m.logger.Info("Status indicator full config updated", "displayMode", string(cfg.DisplayMode), "duration", cfg.Duration)
}

// SetTooltipDelay 设置编码提示延迟显示时间（毫秒）
func (m *Manager) SetTooltipDelay(delay int) {
	m.mu.Lock()
	m.tooltipDelay = delay
	m.mu.Unlock()
	m.logger.Info("Tooltip delay updated", "delay", delay)
}

// SetCandidateLayout 设置候选框布局模式
func (m *Manager) SetCandidateLayout(layout config.CandidateLayout) {
	if m.renderer != nil {
		m.renderer.SetLayout(layout)
		m.logger.Info("Candidate layout updated", "layout", layout)
	}
}

// GetCandidateLayout 返回当前候选框布局; renderer 未初始化时返回零值。
// 由 cmdbar ime.toggle("layout") 读取以便横/纵互切。
func (m *Manager) GetCandidateLayout() config.CandidateLayout {
	if m.renderer == nil {
		return ""
	}
	return m.renderer.GetLayout()
}

// SetHideCandidateWindow 在运行时切换候选窗隐藏开关 (cmdbar
// ime.toggle("candwin") 用)。与初始 UpdateConfig 的同名参数共用一个标志位。
func (m *Manager) SetHideCandidateWindow(hide bool) {
	m.mu.Lock()
	m.hideCandidateWindow = hide
	m.mu.Unlock()
	m.logger.Info("Candidate window visibility toggled", "hidden", hide)
}

// IsHideCandidateWindow 返回当前候选窗是否被强制隐藏。
func (m *Manager) IsHideCandidateWindow() bool {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.hideCandidateWindow
}

// SetGDIFontParams 设置候选框、工具栏和编码提示的GDI字体粗细和缩放
func (m *Manager) SetGDIFontParams(weight int, scale float64) {
	if m.renderer != nil {
		m.renderer.SetGDIFontParams(weight, scale)
	}
	if m.toolbar != nil {
		m.toolbar.SetGDIFontParams(weight, scale)
	}
	if m.tooltip != nil {
		m.tooltip.SetGDIFontParams(weight, scale)
	}
	if m.status != nil {
		// 状态窗口使用较细字重（400=Normal），小尺寸文字避免过粗
		m.status.SetGDIFontParams(400, scale)
	}
	m.logger.Info("GDI font params updated (candidate/toolbar/tooltip)", "weight", weight, "scale", scale)
}

// SetMenuFontParams 设置所有菜单的GDI字体粗细（独立于候选框）
func (m *Manager) SetMenuFontParams(weight int, scale float64) {
	if m.unifiedPopupMenu != nil {
		m.unifiedPopupMenu.SetGDIFontParams(weight, scale)
	}
	if m.toolbar != nil {
		m.toolbar.SetMenuFontParams(weight, scale)
	}
	if m.window != nil {
		m.window.SetMenuFontParams(weight, scale)
	}
	if m.status != nil {
		m.status.SetMenuFontParams(weight, scale)
	}
	m.logger.Info("GDI font params updated (menu)", "weight", weight, "scale", scale)
}

// SetMenuFontSize 设置所有菜单字体大小（DPI缩放前基础值）
func (m *Manager) SetMenuFontSize(size float64) {
	if m.unifiedPopupMenu != nil {
		m.unifiedPopupMenu.SetMenuFontSize(size)
	}
	if m.toolbar != nil {
		m.toolbar.SetMenuFontSize(size)
	}
	if m.window != nil {
		m.window.SetMenuFontSize(size)
	}
	if m.status != nil {
		m.status.SetMenuFontSize(size)
	}
	m.logger.Info("Menu font size updated", "size", size)
}

// SetTextRenderMode 设置文本渲染模式（FontEngineGDI / FontEngineFreetype / FontEngineDirectWrite）
// Manager 是 facade，接受 config 层的 FontEngine 类型，内部映射到 ui 包的 TextRenderMode。
func (m *Manager) SetTextRenderMode(mode config.FontEngine) {
	renderMode := TextRenderModeGDI
	switch mode {
	case config.FontEngineFreetype:
		renderMode = TextRenderModeFreetype
	case config.FontEngineDirectWrite:
		renderMode = TextRenderModeDirectWrite
	}
	if m.renderer != nil {
		m.renderer.SetTextRenderMode(renderMode)
	}
	if m.toolbar != nil {
		m.toolbar.SetTextRenderMode(renderMode)
	}
	if m.tooltip != nil {
		m.tooltip.SetTextRenderMode(renderMode)
	}
	if m.unifiedPopupMenu != nil {
		m.unifiedPopupMenu.SetTextRenderMode(renderMode)
	}
	if m.window != nil {
		m.window.SetTextRenderMode(renderMode)
	}
	if m.status != nil {
		m.status.SetTextRenderMode(renderMode)
	}
	m.logger.Info("Text render mode updated", "mode", mode)
}

// SetHidePreedit 设置是否隐藏预编辑区域
func (m *Manager) SetHidePreedit(hide bool) {
	if m.renderer != nil {
		m.renderer.SetHidePreedit(hide)
		m.logger.Info("Hide preedit updated", "hide", hide)
	}
}

// SetPreeditMode 设置编码显示模式（"top" 或 "embedded"）
func (m *Manager) SetPreeditMode(mode config.PreeditMode) {
	if m.renderer != nil {
		m.renderer.SetPreeditMode(mode)
	}
}

// SetPagerDisplayMode 设置页码显示方式（覆盖主题配置）
func (m *Manager) SetPagerDisplayMode(mode config.PagerDisplayMode) {
	m.pagerDisplayMode = mode
	m.applyPagerOverride()
}

// applyPagerOverride 根据 pagerDisplayMode 覆盖渲染器的翻页显示设置。
// 必须在 renderer.SetTheme() 之后调用，以确保主题值已写入。
func (m *Manager) applyPagerOverride() {
	if m.renderer == nil {
		return
	}
	switch m.pagerDisplayMode {
	case config.PagerDisplayNever:
		m.renderer.SetAlwaysShowPager(false)
		m.renderer.SetShowPageNumber(false)
	case config.PagerDisplayAuto:
		m.renderer.SetAlwaysShowPager(false)
		m.renderer.SetShowPageNumber(true)
	case config.PagerDisplayAlways:
		m.renderer.SetAlwaysShowPager(true)
		m.renderer.SetShowPageNumber(true)
		// PagerDisplayDefault（空字符串）：不覆盖，保留主题值
	}
}

// SetCmdbarCandidatePrefix 设置副作用 cmdbar 候选 (Actions 含 ActionEffect) 的渲染前缀。
// 空字符串表示完全不显示前缀。
func (m *Manager) SetCmdbarCandidatePrefix(prefix string) {
	if m.renderer != nil {
		m.renderer.SetCmdbarPrefix(prefix)
		m.logger.Info("Cmdbar candidate prefix updated", "prefixLen", len(prefix))
	}
}

// SetMaxCandidateChars 设置候选文本最大显示 rune 数（0 表示不限制）
func (m *Manager) SetMaxCandidateChars(n int) {
	m.mu.Lock()
	m.maxCandidateChars = n
	m.mu.Unlock()
	m.logger.Info("Max candidate chars updated", "maxChars", n)
}

// OpenSettings opens the settings window
func (m *Manager) OpenSettings() {
	m.OpenSettingsWithPage("")
}

// OpenSettingsWithPage opens the settings window with a specific page
func (m *Manager) OpenSettingsWithPage(page string) {
	m.mu.Lock()
	if !m.ready {
		m.mu.Unlock()
		return
	}
	m.mu.Unlock()

	select {
	case m.cmdCh <- UICommand{Type: cmdSettings, SettingsPage: page}:
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		m.logger.Warn("UI command channel full, dropping settings command")
	}
}

// doOpenSettings opens the settings window (called from UI thread)
// page parameter can specify a specific page to open (e.g., "about")
func (m *Manager) doOpenSettings(page string) {
	m.logger.Info("Opening settings application", "page", page)

	// Try to launch wind_setting.exe
	// First try the install directory, then fall back to current directory
	shell32 := windows.NewLazySystemDLL("shell32.dll")
	procShellExecuteW := shell32.NewProc("ShellExecuteW")

	// Try paths in order of preference: same directory as current exe first
	settingExe := "wind_setting.exe"
	if buildvariant.IsDebug() {
		settingExe = "wind_setting_debug.exe"
	}
	var paths []string
	if exePath, err := os.Executable(); err == nil {
		paths = append(paths, filepath.Join(filepath.Dir(exePath), settingExe))
	}
	paths = append(paths, settingExe) // Fallback: current directory or PATH

	openPtr, _ := windows.UTF16PtrFromString("open")

	// Prepare parameters if page is specified
	var paramsPtr *uint16
	if page != "" {
		params := "--page=" + page
		paramsPtr, _ = windows.UTF16PtrFromString(params)
	}

	var attempts []settingsLaunchAttempt

	for _, path := range paths {
		pathPtr, _ := windows.UTF16PtrFromString(path)

		var paramsArg uintptr
		if paramsPtr != nil {
			paramsArg = uintptr(unsafe.Pointer(paramsPtr))
		}

		ret, _, err := procShellExecuteW.Call(
			0,                                // hwnd
			uintptr(unsafe.Pointer(openPtr)), // lpOperation ("open")
			uintptr(unsafe.Pointer(pathPtr)), // lpFile (path to exe)
			paramsArg,                        // lpParameters
			0,                                // lpDirectory
			1,                                // nShowCmd (SW_SHOWNORMAL)
		)

		// ShellExecuteW returns >32 on success
		if ret > 32 {
			m.logger.Info("Settings application launched successfully", "path", path, "page", page)
			return
		}

		_, statErr := os.Stat(path)
		attempts = append(attempts, settingsLaunchAttempt{
			path:   path,
			ret:    ret,
			err:    err,
			exists: statErr == nil,
		})
	}

	// 全部路径失败：记录详细信息，便于排查（不再回退到浏览器，因为 127.0.0.1:18923
	// 由 wind_setting.exe 自身提供服务，进程都没起来时浏览器 fallback 必然连不上）。
	last := attempts[len(attempts)-1]
	m.logger.Error("Failed to launch settings application",
		"page", page,
		"settingExe", settingExe,
		"triedPaths", formatAttemptPaths(attempts),
		"lastPath", last.path,
		"lastPathExists", last.exists,
		"ret", last.ret,
		"retMeaning", shellExecuteErrorMeaning(last.ret),
		"error", last.err,
	)
}

// formatAttemptPaths 将多次启动尝试格式化为 "path1(exists)|path2(missing)" 形式，
// 便于在日志中一行内看到所有候选路径及其存在性。
func formatAttemptPaths(attempts []settingsLaunchAttempt) string {
	parts := make([]string, 0, len(attempts))
	for _, a := range attempts {
		state := "missing"
		if a.exists {
			state = "exists"
		}
		parts = append(parts, a.path+"("+state+")")
	}
	return strings.Join(parts, " | ")
}

// shellExecuteErrorMeaning 将 ShellExecuteW 的小于等于 32 的返回值映射为可读说明，
// 便于诊断设置程序无法启动的具体原因。错误码定义见 Win32 ShellExecuteW 文档。
func shellExecuteErrorMeaning(ret uintptr) string {
	switch ret {
	case 0:
		return "out of memory or resources"
	case 2: // ERROR_FILE_NOT_FOUND / SE_ERR_FNF
		return "file not found"
	case 3: // ERROR_PATH_NOT_FOUND / SE_ERR_PNF
		return "path not found"
	case 5: // SE_ERR_ACCESSDENIED
		return "access denied (UAC/SmartScreen/AV blocked or user cancelled)"
	case 8: // SE_ERR_OOM
		return "out of memory"
	case 11: // ERROR_BAD_FORMAT
		return "invalid exe format"
	case 26: // SE_ERR_SHARE
		return "sharing violation"
	case 27: // SE_ERR_ASSOCINCOMPLETE
		return "file association information incomplete"
	case 28: // SE_ERR_DDETIMEOUT
		return "DDE transaction timed out"
	case 29: // SE_ERR_DDEFAIL
		return "DDE transaction failed"
	case 30: // SE_ERR_DDEBUSY
		return "DDE transaction busy"
	case 31: // SE_ERR_NOASSOC
		return "no application associated with the file"
	case 32: // SE_ERR_DLLNOTFOUND
		return "required DLL not found"
	default:
		return "unknown shell execute error"
	}
}
