package ui

import (
	"fmt"
	"unsafe"

	"golang.org/x/sys/windows"

	"github.com/huanfeng/wind_input/pkg/buildvariant"
)

// UpdateConfig 更新 UI 配置（热更新）
func (m *Manager) UpdateConfig(fontSize float64, fontPath string, hideCandidateWindow bool) {
	// 更新渲染器的字体设置
	if m.renderer != nil {
		m.renderer.UpdateFont(fontSize, fontPath)
	}
	if m.toolbar != nil {
		m.toolbar.SetFontPath(fontPath)
	}
	if m.tooltip != nil {
		m.tooltip.SetFontPath(fontPath)
	}
	if m.unifiedPopupMenu != nil {
		m.unifiedPopupMenu.SetFontPath(fontPath)
	}
	if m.window != nil {
		m.window.SetMenuFontPath(fontPath)
	}
	// 更新调试开关
	m.mu.Lock()
	m.hideCandidateWindow = hideCandidateWindow
	m.mu.Unlock()
	m.logger.Info("UI config updated", "fontSize", fontSize, "fontPath", fontPath, "hideCandidateWindow", hideCandidateWindow)
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

// SetTooltipDelay 设置编码提示延迟显示时间（毫秒）
func (m *Manager) SetTooltipDelay(delay int) {
	m.mu.Lock()
	m.tooltipDelay = delay
	m.mu.Unlock()
	m.logger.Info("Tooltip delay updated", "delay", delay)
}

// SetCandidateLayout 设置候选框布局模式
func (m *Manager) SetCandidateLayout(layout string) {
	if m.renderer != nil {
		m.renderer.SetLayout(layout)
		m.logger.Info("Candidate layout updated", "layout", layout)
	}
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
	m.logger.Info("Menu font size updated", "size", size)
}

// SetTextRenderMode 设置文本渲染模式（"gdi"、"freetype" 或 "directwrite"）
func (m *Manager) SetTextRenderMode(mode string) {
	renderMode := TextRenderModeGDI
	switch mode {
	case "freetype":
		renderMode = TextRenderModeFreetype
	case "directwrite":
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
	m.logger.Info("Text render mode updated", "mode", mode)
}

// SetHidePreedit 设置是否隐藏预编辑区域
func (m *Manager) SetHidePreedit(hide bool) {
	if m.renderer != nil {
		m.renderer.SetHidePreedit(hide)
		m.logger.Info("Hide preedit updated", "hide", hide)
	}
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
	case m.cmdCh <- UICommand{Type: "settings", SettingsPage: page}:
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

	// Try paths in order of preference
	settingExe := "wind_setting.exe"
	if buildvariant.IsDebug() {
		settingExe = "wind_setting_debug.exe"
	}
	paths := []string{
		fmt.Sprintf("C:\\Program Files\\%s\\%s", buildvariant.AppName(), settingExe),
		settingExe, // Current directory or PATH
	}

	openPtr, _ := windows.UTF16PtrFromString("open")

	// Prepare parameters if page is specified
	var paramsPtr *uint16
	if page != "" {
		params := "--page=" + page
		paramsPtr, _ = windows.UTF16PtrFromString(params)
	}

	var lastRet uintptr
	var lastErr error

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

		lastRet = ret
		lastErr = err

		// ShellExecuteW returns >32 on success
		if ret > 32 {
			m.logger.Info("Settings application launched successfully", "path", path, "page", page)
			return
		}
	}

	// All paths failed, fall back to opening the web URL
	m.logger.Warn("Failed to launch wind_setting.exe, falling back to web URL", "ret", lastRet, "error", lastErr)

	// Build URL with page parameter
	url := "http://127.0.0.1:18923"
	if page != "" {
		url += "/#/" + page
	}
	urlPtr, _ := windows.UTF16PtrFromString(url)

	ret, _, err := procShellExecuteW.Call(
		0,                                // hwnd
		uintptr(unsafe.Pointer(openPtr)), // lpOperation ("open")
		uintptr(unsafe.Pointer(urlPtr)),  // lpFile (URL)
		0,                                // lpParameters
		0,                                // lpDirectory
		1,                                // nShowCmd (SW_SHOWNORMAL)
	)

	if ret <= 32 {
		m.logger.Error("Failed to open settings URL", "error", err, "ret", ret)
	} else {
		m.logger.Info("Settings URL opened successfully (fallback)", "url", url)
	}
}
