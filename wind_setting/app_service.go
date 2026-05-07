package main

import (
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"time"
	"unsafe"

	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/rpcapi"
	"github.com/huanfeng/wind_input/pkg/systemfont"
	"github.com/huanfeng/wind_input/pkg/theme"
	wailsRuntime "github.com/wailsapp/wails/v2/pkg/runtime"
	"golang.org/x/sys/windows"
)

var (
	modShell32        = windows.NewLazySystemDLL("shell32.dll")
	procShellExecuteW = modShell32.NewProc("ShellExecuteW")
)

// shellOpen 通过 ShellExecuteW 打开文件、目录或 URL。
// 相比 explorer.exe / rundll32，ShellExecuteW 直接调用系统 shell，
// 对所有默认浏览器均兼容，且不存在 cmd shell 注入风险。
func shellOpen(path string) error {
	verbPtr, err := windows.UTF16PtrFromString("open")
	if err != nil {
		return err
	}
	pathPtr, err := windows.UTF16PtrFromString(path)
	if err != nil {
		return err
	}
	ret, _, _ := procShellExecuteW.Call(
		0,
		uintptr(unsafe.Pointer(verbPtr)),
		uintptr(unsafe.Pointer(pathPtr)),
		0, 0,
		uintptr(windows.SW_SHOWNORMAL),
	)
	if ret <= 32 {
		return fmt.Errorf("ShellExecuteW 失败，错误码: %d", ret)
	}
	return nil
}

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

// DumpPerf 导出按键链路性能样本到 JSONL 文件
func (a *App) DumpPerf(path string, clear bool) (*rpcapi.SystemDumpPerfReply, error) {
	if a.rpcClient == nil {
		return nil, fmt.Errorf("RPC 客户端未初始化")
	}
	return a.rpcClient.SystemDumpPerf(path, clear)
}

// GetPerfStats 获取当前性能采样统计摘要（不落盘）
func (a *App) GetPerfStats() (*rpcapi.SystemPerfStatsReply, error) {
	if a.rpcClient == nil {
		return nil, fmt.Errorf("RPC 客户端未初始化")
	}
	return a.rpcClient.SystemGetPerfStats()
}

// ReadPerfFile 将性能样本导出到临时文件并读取内容，随后删除临时文件。
// 返回的 Path 仅用于前端判断数据是否可用。
func (a *App) ReadPerfFile() (*rpcapi.SystemDumpPerfReply, error) {
	if a.rpcClient == nil {
		return nil, fmt.Errorf("RPC 客户端未初始化")
	}

	tmpDir, err := os.MkdirTemp("", "windinput-perf-*")
	if err != nil {
		return nil, fmt.Errorf("创建临时目录失败: %w", err)
	}
	defer os.RemoveAll(tmpDir)

	tmpPath := filepath.Join(tmpDir, "perf.jsonl")
	reply, err := a.rpcClient.SystemDumpPerf(tmpPath, false)
	if err != nil {
		return nil, err
	}

	data, err := os.ReadFile(tmpPath)
	if err != nil {
		return nil, fmt.Errorf("读取临时文件失败: %w", err)
	}
	reply.Content = string(data)
	return reply, nil
}

// ExportPerfData 弹出系统保存对话框，让用户选择路径后导出性能样本。
func (a *App) ExportPerfData() (*rpcapi.SystemDumpPerfReply, error) {
	if a.rpcClient == nil {
		return nil, fmt.Errorf("RPC 客户端未初始化")
	}

	defaultFilename := fmt.Sprintf("perf_%s.jsonl", time.Now().Format("20060102_150405"))
	path, err := wailsRuntime.SaveFileDialog(a.ctx, wailsRuntime.SaveDialogOptions{
		Title:           "导出性能诊断数据",
		DefaultFilename: defaultFilename,
		Filters: []wailsRuntime.FileFilter{
			{DisplayName: "JSONL 文件 (*.jsonl)", Pattern: "*.jsonl"},
		},
	})
	if err != nil {
		return nil, fmt.Errorf("打开保存对话框失败: %w", err)
	}
	if path == "" {
		return &rpcapi.SystemDumpPerfReply{Cancelled: true}, nil
	}

	return a.rpcClient.SystemDumpPerf(path, false)
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

	// 配置变更通过 RPC 事件推送，不再轮询文件
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
	if a.rpcClient != nil {
		if reply, err := a.rpcClient.ConfigGet([]string{"ui.theme"}); err == nil {
			if val, ok := reply.Values["ui.theme"]; ok {
				if s, ok := val.(string); ok && s != "" {
					currentTheme = s
				}
			}
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

// PathInfo contains display-friendly path information for the settings UI.
type PathInfo struct {
	ConfigDir        string `json:"config_dir"`
	ConfigDirDisplay string `json:"config_dir_display"`
	LogsDir          string `json:"logs_dir"`
	LogsDirDisplay   string `json:"logs_dir_display"`
	IsPortable       bool   `json:"is_portable"`
}

// GetPathInfo returns the actual and display paths for config and logs directories.
func (a *App) GetPathInfo() (*PathInfo, error) {
	configDir, err := config.GetConfigDir()
	if err != nil {
		return nil, err
	}
	logsDir, err := config.GetLogsDir()
	if err != nil {
		return nil, err
	}
	return &PathInfo{
		ConfigDir:        configDir,
		ConfigDirDisplay: config.GetConfigDirDisplay(),
		LogsDir:          logsDir,
		LogsDirDisplay:   config.GetLogsDirDisplay(),
		IsPortable:       config.IsPortableMode(),
	}, nil
}

// OpenLogFolder opens the log directory in the system file explorer.
func (a *App) OpenLogFolder() error {
	path, err := config.GetLogsDir()
	if err != nil {
		wailsRuntime.LogErrorf(a.ctx, "[setting] 获取日志目录失败: %v", err)
		return err
	}
	// 日志目录在首次写入前可能尚未创建，提前确保其存在
	if err := os.MkdirAll(path, 0o755); err != nil {
		wailsRuntime.LogErrorf(a.ctx, "[setting] 创建日志目录失败: %v", err)
		return err
	}
	wailsRuntime.LogInfof(a.ctx, "[setting] 打开日志目录 len=%d", len(path))
	if err := shellOpen(path); err != nil {
		wailsRuntime.LogErrorf(a.ctx, "[setting] 打开日志目录失败: %v", err)
		return err
	}
	return nil
}

// OpenConfigFolder opens the config directory in the system file explorer.
func (a *App) OpenConfigFolder() error {
	path, err := config.GetConfigDir()
	if err != nil {
		wailsRuntime.LogErrorf(a.ctx, "[setting] 获取配置目录失败: %v", err)
		return err
	}
	wailsRuntime.LogInfof(a.ctx, "[setting] 打开配置目录 len=%d", len(path))
	if err := shellOpen(path); err != nil {
		wailsRuntime.LogErrorf(a.ctx, "[setting] 打开配置目录失败: %v", err)
		return err
	}
	return nil
}

// OpenExternalURL opens an external URL in the default browser.
func (a *App) OpenExternalURL(url string) error {
	if url == "" {
		return fmt.Errorf("empty url")
	}
	wailsRuntime.LogInfof(a.ctx, "[setting] 打开外部链接 len=%d", len(url))
	if err := shellOpen(url); err != nil {
		wailsRuntime.LogErrorf(a.ctx, "[setting] 打开外部链接失败: %v", err)
		return err
	}
	return nil
}

// ========== 数据目录管理 ==========

// SelectDataDir 打开目录选择对话框
func (a *App) SelectDataDir() (string, error) {
	return wailsRuntime.OpenDirectoryDialog(a.ctx, wailsRuntime.OpenDialogOptions{
		Title: "选择数据存储目录",
	})
}

// DataDirInfo 数据目录信息
type DataDirInfo struct {
	CurrentDir string `json:"current_dir"`
	SizeBytes  int64  `json:"size_bytes"`
	SizeText   string `json:"size_text"`
	FileCount  int    `json:"file_count"`
}

// GetDataDirInfo 获取当前数据目录信息（路径、大小、文件数）
func (a *App) GetDataDirInfo() (*DataDirInfo, error) {
	dir, err := config.GetConfigDir()
	if err != nil {
		return nil, err
	}

	var totalSize int64
	var fileCount int
	_ = filepath.Walk(dir, func(_ string, info os.FileInfo, err error) error {
		if err != nil {
			return nil
		}
		if !info.IsDir() {
			totalSize += info.Size()
			fileCount++
		}
		return nil
	})

	return &DataDirInfo{
		CurrentDir: dir,
		SizeBytes:  totalSize,
		SizeText:   formatSize(totalSize),
		FileCount:  fileCount,
	}, nil
}

func formatSize(bytes int64) string {
	const (
		KB = 1024
		MB = KB * 1024
		GB = MB * 1024
	)
	switch {
	case bytes >= GB:
		return fmt.Sprintf("%.1f GB", float64(bytes)/float64(GB))
	case bytes >= MB:
		return fmt.Sprintf("%.1f MB", float64(bytes)/float64(MB))
	case bytes >= KB:
		return fmt.Sprintf("%.1f KB", float64(bytes)/float64(KB))
	default:
		return fmt.Sprintf("%d B", bytes)
	}
}

// DataDirValidation 数据目录验证结果
type DataDirValidation struct {
	Valid   bool   `json:"valid"`
	Warning string `json:"warning"`
	IsEmpty bool   `json:"is_empty"`
	IsSame  bool   `json:"is_same"`
}

// ValidateDataDirPath 验证数据目录路径
func (a *App) ValidateDataDirPath(path string) (*DataDirValidation, error) {
	valid, warning := config.ValidateDataDirPath(path)
	result := &DataDirValidation{Valid: valid, Warning: warning}

	if !valid {
		return result, nil
	}

	// 检查是否与当前目录相同
	currentDir, err := config.GetConfigDir()
	if err == nil {
		cleanCurrent := filepath.Clean(currentDir)
		cleanNew := filepath.Clean(path)
		if strings.EqualFold(cleanCurrent, cleanNew) {
			result.IsSame = true
			result.Valid = false
			result.Warning = "与当前数据目录相同"
			return result, nil
		}
	}

	// 检查目录是否为空
	result.IsEmpty = true
	if entries, err := os.ReadDir(path); err == nil && len(entries) > 0 {
		result.IsEmpty = false
	}

	return result, nil
}

// ChangeDataDirRequest 切换数据目录请求
type ChangeDataDirRequest struct {
	NewPath       string `json:"new_path"`
	Migrate       bool   `json:"migrate"`
	Overwrite     bool   `json:"overwrite"`
	DeleteOldData bool   `json:"delete_old_data"`
}

// ChangeDataDirResult 切换数据目录结果
type ChangeDataDirResult struct {
	Success  bool     `json:"success"`
	Warnings []string `json:"warnings"`
}

// ChangeUserDataDir 切换用户数据目录
func (a *App) ChangeUserDataDir(req ChangeDataDirRequest) (*ChangeDataDirResult, error) {
	valid, warning := config.ValidateDataDirPath(req.NewPath)
	if !valid {
		return nil, fmt.Errorf("路径验证失败: %s", warning)
	}

	// 确保目标目录存在
	if err := os.MkdirAll(req.NewPath, 0755); err != nil {
		return nil, fmt.Errorf("无法创建目录: %w", err)
	}

	currentDir, err := config.GetConfigDir()
	if err != nil {
		return nil, fmt.Errorf("无法获取当前数据目录: %w", err)
	}

	result := &ChangeDataDirResult{Success: true}

	// 暂停服务：释放数据库文件锁，保持进程和 RPC 通道
	serviceRunning := a.rpcClient.IsAvailable()
	if serviceRunning {
		if err := a.rpcClient.SystemPause(); err != nil {
			return nil, fmt.Errorf("无法暂停输入法服务: %w", err)
		}
	}

	if req.Migrate {
		if err := migrateAllData(currentDir, req.NewPath, req.Overwrite); err != nil {
			// 迁移失败，恢复服务
			if serviceRunning {
				_ = a.rpcClient.SystemResume("")
			}
			return nil, fmt.Errorf("数据迁移失败: %w", err)
		}

		if req.DeleteOldData {
			if err := clearDirContents(currentDir); err != nil {
				result.Warnings = append(result.Warnings,
					fmt.Sprintf("旧目录中的文件未能完全删除，请稍后手动清理：%s", currentDir))
			}
		}
	}

	// 写入 datadir.conf
	if err := config.WriteUserDataDirOverride(req.NewPath); err != nil {
		if serviceRunning {
			_ = a.rpcClient.SystemResume("")
		}
		return nil, fmt.Errorf("写入配置失败: %w", err)
	}

	// 恢复服务（使用新数据目录）
	if serviceRunning {
		if err := a.rpcClient.SystemResume(req.NewPath); err != nil {
			result.Warnings = append(result.Warnings,
				fmt.Sprintf("服务恢复失败: %v，请手动重启输入法", err))
		}
	}

	return result, nil
}

// migrateAllData 迁移所有用户数据文件
// overwrite 为 true 时覆盖目标中的同名文件
func migrateAllData(srcDir, dstDir string, overwrite bool) error {
	entries, err := os.ReadDir(srcDir)
	if err != nil {
		return fmt.Errorf("无法读取源目录: %w", err)
	}

	for _, entry := range entries {
		src := filepath.Join(srcDir, entry.Name())
		dst := filepath.Join(dstDir, entry.Name())

		if entry.IsDir() {
			if err := copyDirRecursive(src, dst, overwrite); err != nil {
				return fmt.Errorf("复制目录 %s 失败: %w", entry.Name(), err)
			}
		} else {
			// 目标已存在且不覆盖，跳过
			if !overwrite {
				if _, err := os.Stat(dst); err == nil {
					continue
				}
			}
			if err := copyFileSimple(src, dst); err != nil {
				return fmt.Errorf("复制文件 %s 失败: %w", entry.Name(), err)
			}
		}
	}
	return nil
}

// clearDirContents 删除目录内的所有文件和子目录，但保留目录本身
func clearDirContents(dir string) error {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return err
	}
	var lastErr error
	for _, entry := range entries {
		p := filepath.Join(dir, entry.Name())
		if err := os.RemoveAll(p); err != nil {
			lastErr = err
		}
	}
	return lastErr
}

func copyFileSimple(src, dst string) error {
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()

	out, err := os.Create(dst)
	if err != nil {
		return err
	}
	defer out.Close()

	_, err = io.Copy(out, in)
	return err
}

func copyDirRecursive(src, dst string, overwrite bool) error {
	return filepath.Walk(src, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		rel, err := filepath.Rel(src, path)
		if err != nil {
			return err
		}
		target := filepath.Join(dst, rel)
		if info.IsDir() {
			return os.MkdirAll(target, 0755)
		}
		if !overwrite {
			if _, err := os.Stat(target); err == nil {
				return nil // 目标已存在，跳过
			}
		}
		return copyFileSimple(path, target)
	})
}
