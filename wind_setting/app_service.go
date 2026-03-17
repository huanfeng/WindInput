package main

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"

	"github.com/huanfeng/wind_input/pkg/control"
	"github.com/huanfeng/wind_input/pkg/theme"
)

// ========== 控制管道 ==========

// CheckServiceRunning 检查服务是否运行
func (a *App) CheckServiceRunning() (bool, error) {
	return a.controlClient.IsServiceRunning(), nil
}

// NotifyReload 通知服务重载
func (a *App) NotifyReload(target string) error {
	return a.controlClient.NotifyReload(target)
}

// GetServiceStatus 获取服务状态
func (a *App) GetServiceStatus() (*control.ServiceStatus, error) {
	return a.controlClient.GetStatus()
}

// ========== 文件变化检测 ==========

// FileChangeStatus 文件变化状态
type FileChangeStatus struct {
	ConfigChanged   bool `json:"config_changed"`
	PhrasesChanged  bool `json:"phrases_changed"`
	ShadowChanged   bool `json:"shadow_changed"`
	UserDictChanged bool `json:"userdict_changed"`
}

// CheckAllFilesModified 检查所有文件是否被外部修改
func (a *App) CheckAllFilesModified() (*FileChangeStatus, error) {
	status := &FileChangeStatus{}

	if changed, _ := a.CheckConfigModified(); changed {
		status.ConfigChanged = true
	}
	if changed, _ := a.CheckPhrasesModified(); changed {
		status.PhrasesChanged = true
	}
	if a.shadowEditor != nil {
		if changed, _ := a.shadowEditor.HasChanged(); changed {
			status.ShadowChanged = true
		}
	}
	if changed, _ := a.CheckUserDictModified(); changed {
		status.UserDictChanged = true
	}

	return status, nil
}

// ReloadAllFiles 重新加载所有文件
func (a *App) ReloadAllFiles() error {
	var lastErr error

	if err := a.ReloadConfig(); err != nil {
		lastErr = err
	}
	if err := a.ReloadPhrases(); err != nil {
		lastErr = err
	}
	if a.shadowEditor != nil {
		if err := a.shadowEditor.Reload(); err != nil {
			lastErr = err
		}
	}
	if err := a.ReloadUserDict(); err != nil {
		lastErr = err
	}

	return lastErr
}

// ========== 主题管理 ==========

// ThemeInfo 主题信息（用于前端）
type ThemeInfo struct {
	Name        string `json:"name"`
	DisplayName string `json:"display_name"`
	Author      string `json:"author"`
	Version     string `json:"version"`
	IsBuiltin   bool   `json:"is_builtin"`
	IsActive    bool   `json:"is_active"`
}

// GetAvailableThemes 获取可用的主题列表
func (a *App) GetAvailableThemes() ([]ThemeInfo, error) {
	themeManager := theme.NewManager(nil)
	themeNames := themeManager.ListAvailableThemes()

	// 获取当前配置的主题
	currentTheme := "default"
	if a.configEditor != nil {
		cfg := a.configEditor.GetConfig()
		if cfg != nil && cfg.UI.Theme != "" {
			currentTheme = cfg.UI.Theme
		}
	}

	themes := make([]ThemeInfo, 0, len(themeNames))
	for _, name := range themeNames {
		info := ThemeInfo{
			Name:     name,
			IsActive: name == currentTheme,
		}

		// 加载主题以获取显示名称
		if err := themeManager.LoadTheme(name); err == nil {
			t := themeManager.GetCurrentTheme()
			if t != nil {
				info.DisplayName = t.Meta.Name
				info.Author = t.Meta.Author
				info.Version = t.Meta.Version
			}
		}

		if info.DisplayName == "" {
			info.DisplayName = name
		}

		themes = append(themes, info)
	}

	return themes, nil
}

// GetThemePreview 获取主题预览数据（颜色配置）
func (a *App) GetThemePreview(themeName string) (map[string]interface{}, error) {
	themeManager := theme.NewManager(nil)

	if err := themeManager.LoadTheme(themeName); err != nil {
		return nil, fmt.Errorf("failed to load theme: %w", err)
	}

	t := themeManager.GetCurrentTheme()
	if t == nil {
		return nil, fmt.Errorf("theme not found")
	}

	// 返回主题的颜色配置供前端预览
	preview := map[string]interface{}{
		"meta": map[string]string{
			"name":    t.Meta.Name,
			"version": t.Meta.Version,
			"author":  t.Meta.Author,
		},
		"candidate_window": map[string]string{
			"background_color": t.CandidateWindow.BackgroundColor,
			"border_color":     t.CandidateWindow.BorderColor,
			"text_color":       t.CandidateWindow.TextColor,
			"index_color":      t.CandidateWindow.IndexColor,
			"index_bg_color":   t.CandidateWindow.IndexBgColor,
			"hover_bg_color":   t.CandidateWindow.HoverBgColor,
		},
		"toolbar": map[string]string{
			"background_color":       t.Toolbar.BackgroundColor,
			"border_color":           t.Toolbar.BorderColor,
			"mode_chinese_bg_color":  t.Toolbar.ModeChineseBgColor,
			"mode_english_bg_color":  t.Toolbar.ModeEnglishBgColor,
			"full_width_on_bg_color": t.Toolbar.FullWidthOnBgColor,
			"punct_chinese_bg_color": t.Toolbar.PunctChineseBgColor,
		},
		"style": map[string]string{
			"index_style":      t.Style.IndexStyle,
			"accent_bar_color": t.Style.AccentBarColor,
		},
	}

	return preview, nil
}

// ========== 工具方法 ==========

// OpenLogFolder opens the log directory in the system file explorer.
func (a *App) OpenLogFolder() error {
	base := os.Getenv("LOCALAPPDATA")
	if base == "" {
		return fmt.Errorf("LOCALAPPDATA not set")
	}
	path := filepath.Join(base, "WindInput", "logs")
	return exec.Command("explorer.exe", path).Start()
}

// OpenExternalURL opens an external URL in the default browser.
func (a *App) OpenExternalURL(url string) error {
	if url == "" {
		return fmt.Errorf("empty url")
	}
	return exec.Command("rundll32", "url.dll,FileProtocolHandler", url).Start()
}
