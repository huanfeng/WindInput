package main

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"

	"github.com/huanfeng/wind_input/pkg/buildvariant"
	"github.com/huanfeng/wind_input/pkg/rpcapi"
	"github.com/huanfeng/wind_input/pkg/systemfont"
	"github.com/huanfeng/wind_input/pkg/theme"
)

// ========== 服务通信 ==========

// CheckServiceRunning 检查服务是否运行
func (a *App) CheckServiceRunning() (bool, error) {
	return a.rpcClient.IsAvailable(), nil
}

// NotifyReload 通知服务重载
func (a *App) NotifyReload(target string) error {
	return a.rpcClient.SystemNotifyReload(target)
}

// GetServiceStatus 获取服务状态
func (a *App) GetServiceStatus() (*rpcapi.SystemStatusReply, error) {
	return a.rpcClient.SystemGetStatus()
}

// ResetUserData 重置用户数据（清除用户词库、临时词库、Shadow 规则、词频）
// schemaID 为空时清除所有方案的数据
func (a *App) ResetUserData(schemaID string) error {
	if a.rpcClient == nil {
		return fmt.Errorf("RPC 客户端未初始化")
	}
	return a.rpcClient.SystemResetDB(schemaID)
}

// DeleteSchemaData 彻底删除方案的存储 bucket（用于清理残留方案）
func (a *App) DeleteSchemaData(schemaID string) error {
	if a.rpcClient == nil {
		return fmt.Errorf("RPC 客户端未初始化")
	}
	return a.rpcClient.SystemDeleteSchema(schemaID)
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
	HasVariants bool   `json:"has_variants"` // 是否支持亮暗双模式
}

// SystemFontInfo 系统字体信息（用于设置页下拉选择）
type SystemFontInfo struct {
	Family      string `json:"family"`
	DisplayName string `json:"display_name"`
}

// GetSystemFonts 获取系统字体族列表
func (a *App) GetSystemFonts() ([]SystemFontInfo, error) {
	fonts, err := systemfont.List()
	if err != nil && len(fonts) == 0 {
		return nil, err
	}

	result := make([]SystemFontInfo, 0, len(fonts))
	for _, font := range fonts {
		result = append(result, SystemFontInfo{
			Family:      font.Family,
			DisplayName: font.DisplayName,
		})
	}
	return result, nil
}

// GetAvailableThemes 获取可用的主题列表（按排序字段排序）
func (a *App) GetAvailableThemes() ([]ThemeInfo, error) {
	themeManager := theme.NewManager(nil)

	// 使用 ListAvailableThemeInfos 获取排序后的主题列表
	sortedInfos := themeManager.ListAvailableThemeInfos()

	// 获取当前配置的主题
	currentTheme := "default"
	if a.configEditor != nil {
		cfg := a.configEditor.GetConfig()
		if cfg != nil && cfg.UI.Theme != "" {
			currentTheme = cfg.UI.Theme
		}
	}

	themes := make([]ThemeInfo, 0, len(sortedInfos))
	for _, si := range sortedInfos {
		info := ThemeInfo{
			Name:        si.ID,
			DisplayName: si.DisplayName,
			IsActive:    si.ID == currentTheme,
			IsBuiltin:   theme.BuiltinThemeIDs[si.ID],
		}

		// 加载主题以获取详细信息
		if err := themeManager.LoadTheme(si.ID); err == nil {
			t := themeManager.GetCurrentTheme()
			if t != nil {
				info.Author = t.Meta.Author
				info.Version = t.Meta.Version
				info.HasVariants = t.HasVariants()
				if t.Meta.Name != "" {
					info.DisplayName = t.Meta.Name
				}
			}
		}

		if info.DisplayName == "" {
			info.DisplayName = si.ID
		}

		themes = append(themes, info)
	}

	return themes, nil
}

// GetThemePreview 获取主题预览数据（颜色配置）
// themeStyle 参数：传入 "system"/"light"/"dark" 以选择对应变体预览
func (a *App) GetThemePreview(themeName string, themeStyle string) (map[string]interface{}, error) {
	themeManager := theme.NewManager(nil)

	if err := themeManager.LoadTheme(themeName); err != nil {
		return nil, fmt.Errorf("failed to load theme: %w", err)
	}

	t := themeManager.GetCurrentTheme()
	if t == nil {
		return nil, fmt.Errorf("theme not found")
	}

	// 根据传入的 themeStyle 确定使用亮色还是暗色变体
	if themeStyle == "" {
		themeStyle = "system"
	}
	isDark := false
	switch themeStyle {
	case "dark":
		isDark = true
	case "light":
		isDark = false
	default: // "system"
		isDark = theme.IsSystemDarkMode()
	}

	// 使用变体系统获取当前模式的颜色
	v := t.GetVariant(isDark)

	// 返回完整的颜色配置供前端预览
	preview := map[string]interface{}{
		"meta": map[string]string{
			"name":    t.Meta.Name,
			"version": t.Meta.Version,
			"author":  t.Meta.Author,
		},
		"candidate_window": map[string]string{
			"background_color":  v.CandidateWindow.BackgroundColor,
			"border_color":      v.CandidateWindow.BorderColor,
			"text_color":        v.CandidateWindow.TextColor,
			"index_color":       v.CandidateWindow.IndexColor,
			"index_bg_color":    v.CandidateWindow.IndexBgColor,
			"hover_bg_color":    v.CandidateWindow.HoverBgColor,
			"selected_bg_color": v.CandidateWindow.SelectedBgColor,
			"input_bg_color":    v.CandidateWindow.InputBgColor,
			"input_text_color":  v.CandidateWindow.InputTextColor,
			"comment_color":     v.CandidateWindow.CommentColor,
			"shadow_color":      v.CandidateWindow.ShadowColor,
		},
		"toolbar": map[string]string{
			"background_color":        v.Toolbar.BackgroundColor,
			"border_color":            v.Toolbar.BorderColor,
			"grip_color":              v.Toolbar.GripColor,
			"mode_chinese_bg_color":   v.Toolbar.ModeChineseBgColor,
			"mode_english_bg_color":   v.Toolbar.ModeEnglishBgColor,
			"mode_text_color":         v.Toolbar.ModeTextColor,
			"full_width_on_bg_color":  v.Toolbar.FullWidthOnBgColor,
			"full_width_off_bg_color": v.Toolbar.FullWidthOffBgColor,
			"full_width_on_color":     v.Toolbar.FullWidthOnColor,
			"full_width_off_color":    v.Toolbar.FullWidthOffColor,
			"punct_chinese_bg_color":  v.Toolbar.PunctChineseBgColor,
			"punct_english_bg_color":  v.Toolbar.PunctEnglishBgColor,
			"punct_chinese_color":     v.Toolbar.PunctChineseColor,
			"punct_english_color":     v.Toolbar.PunctEnglishColor,
			"settings_bg_color":       v.Toolbar.SettingsBgColor,
			"settings_icon_color":     v.Toolbar.SettingsIconColor,
		},
		"style": map[string]string{
			"index_style":      t.Style.IndexStyle,
			"accent_bar_color": t.Style.AccentBarColor,
		},
		"is_dark": map[string]bool{
			"active": isDark,
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
	path := filepath.Join(base, buildvariant.AppName(), "logs")
	return exec.Command("explorer.exe", path).Start()
}

// OpenConfigFolder opens the config directory in the system file explorer.
func (a *App) OpenConfigFolder() error {
	base := os.Getenv("APPDATA")
	if base == "" {
		return fmt.Errorf("APPDATA not set")
	}
	path := filepath.Join(base, buildvariant.AppName())
	return exec.Command("explorer.exe", path).Start()
}

// OpenExternalURL opens an external URL in the default browser.
func (a *App) OpenExternalURL(url string) error {
	if url == "" {
		return fmt.Errorf("empty url")
	}
	return exec.Command("rundll32", "url.dll,FileProtocolHandler", url).Start()
}
