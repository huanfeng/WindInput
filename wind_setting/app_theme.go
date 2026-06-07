package main

import (
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/theme"
	wailsRuntime "github.com/wailsapp/wails/v2/pkg/runtime"
	"gopkg.in/yaml.v3"
)

// ImportThemeResult 主题导入结果
type ImportThemeResult struct {
	Success   bool   `json:"success"`
	Cancelled bool   `json:"cancelled"`
	ThemeName string `json:"theme_name"`
	Conflict  bool   `json:"conflict"`
	ErrorMsg  string `json:"error_msg"`
}

// ImportThemeFromFile 打开系统文件选择对话框，读取并导入 yaml 主题文件。
// force=true 时覆盖同名主题。
func (a *App) ImportThemeFromFile(force bool) ImportThemeResult {
	path, err := wailsRuntime.OpenFileDialog(a.ctx, wailsRuntime.OpenDialogOptions{
		Title: "选择主题文件",
		Filters: []wailsRuntime.FileFilter{
			{DisplayName: "YAML 主题文件 (*.yaml)", Pattern: "*.yaml"},
		},
	})
	if err != nil {
		return ImportThemeResult{ErrorMsg: "打开文件对话框失败: " + err.Error()}
	}
	if path == "" {
		return ImportThemeResult{Cancelled: true}
	}

	content, err := os.ReadFile(path)
	if err != nil {
		return ImportThemeResult{ErrorMsg: "读取文件失败: " + err.Error()}
	}

	return importThemeFromContent(content, force)
}

// ImportThemeFromURL 从指定 URL 下载并导入 yaml 主题文件。
// force=true 时覆盖同名主题。
func (a *App) ImportThemeFromURL(rawURL string, force bool) ImportThemeResult {
	rawURL = strings.TrimSpace(rawURL)
	if rawURL == "" {
		return ImportThemeResult{ErrorMsg: "URL 不能为空"}
	}
	if !strings.HasPrefix(rawURL, "http://") && !strings.HasPrefix(rawURL, "https://") {
		return ImportThemeResult{ErrorMsg: "仅支持 http/https 链接"}
	}

	client := &http.Client{Timeout: 15 * time.Second}
	resp, err := client.Get(rawURL) //nolint:noctx
	if err != nil {
		return ImportThemeResult{ErrorMsg: "下载失败: " + err.Error()}
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return ImportThemeResult{ErrorMsg: fmt.Sprintf("下载失败，服务器返回 %d", resp.StatusCode)}
	}

	const maxSize = 1 << 20 // 1 MB
	content, err := io.ReadAll(io.LimitReader(resp.Body, maxSize))
	if err != nil {
		return ImportThemeResult{ErrorMsg: "读取内容失败: " + err.Error()}
	}

	return importThemeFromContent(content, force)
}

// ThemeURLPreview 主题 URL 预览结果（确认框展示用）。
// YAML 字段回传原始内容，确认导入时走 ImportThemeFromText，避免二次下载。
type ThemeURLPreview struct {
	OK          bool   `json:"ok"`
	Name        string `json:"name"`
	Author      string `json:"author"`
	Version     string `json:"version"`
	Description string `json:"description"`
	SourceURL   string `json:"source_url"`
	YAML        string `json:"yaml"`
	ErrorMsg    string `json:"error_msg"`
}

// parseThemePreviewMeta 从 YAML 内容提取 meta 字段（不校验完整性）。
func parseThemePreviewMeta(content []byte) (name, author, version string) {
	var t theme.Theme
	if err := yaml.Unmarshal(content, &t); err != nil {
		return "", "", ""
	}
	return t.Meta.Name, t.Meta.Author, t.Meta.Version
}

// PreviewThemeFromURL 下载并解析主题 meta（不落盘），供 URL schema 确认框展示。
func (a *App) PreviewThemeFromURL(rawURL string) ThemeURLPreview {
	rawURL = strings.TrimSpace(rawURL)
	if !strings.HasPrefix(rawURL, "https://") {
		return ThemeURLPreview{ErrorMsg: "仅支持 https 链接"}
	}
	client := &http.Client{Timeout: 15 * time.Second}
	resp, err := client.Get(rawURL) //nolint:noctx
	if err != nil {
		return ThemeURLPreview{ErrorMsg: "下载失败: " + err.Error()}
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return ThemeURLPreview{ErrorMsg: fmt.Sprintf("下载失败，服务器返回 %d", resp.StatusCode)}
	}
	const maxSize = 1 << 20
	content, err := io.ReadAll(io.LimitReader(resp.Body, maxSize))
	if err != nil {
		return ThemeURLPreview{ErrorMsg: "读取内容失败: " + err.Error()}
	}
	name, author, version := parseThemePreviewMeta(content)
	if name == "" {
		return ThemeURLPreview{ErrorMsg: "主题缺少 meta.name 或格式错误"}
	}
	var t theme.Theme
	_ = yaml.Unmarshal(content, &t)
	return ThemeURLPreview{
		OK: true, Name: name, Author: author, Version: version,
		Description: t.Meta.Description, SourceURL: rawURL, YAML: string(content),
	}
}

// ImportThemeFromText 校验并导入粘贴的 YAML 文本内容。
// force=true 时覆盖同名主题。
func (a *App) ImportThemeFromText(yamlContent string, force bool) ImportThemeResult {
	if strings.TrimSpace(yamlContent) == "" {
		return ImportThemeResult{ErrorMsg: "内容不能为空"}
	}
	return importThemeFromContent([]byte(yamlContent), force)
}

// importThemeFromContent 统一校验写入管线：解析 → 校验 → 冲突检测 → 写入。
func importThemeFromContent(content []byte, force bool) ImportThemeResult {
	userThemesDir, err := config.GetThemesUserDir()
	if err != nil {
		return ImportThemeResult{ErrorMsg: "获取用户主题目录失败: " + err.Error()}
	}
	return importThemeToDir(content, force, userThemesDir)
}

// importThemeToDir 是 importThemeFromContent 的核心实现，userThemesDir 由调用方传入（便于测试）。
func importThemeToDir(content []byte, force bool, userThemesDir string) ImportThemeResult {
	// 1. 解析 YAML
	t := &theme.Theme{}
	if err := yaml.Unmarshal(content, t); err != nil {
		return ImportThemeResult{ErrorMsg: "YAML 格式错误: " + err.Error()}
	}

	// 2. meta.name 必填
	if t.Meta.Name == "" {
		return ImportThemeResult{ErrorMsg: "主题缺少 meta.name 字段"}
	}

	// 3. 写入临时目录，用 LightweightManager 全链校验（base 存在性 + token 引用完整性）
	tmpDir, err := os.MkdirTemp("", "windinput-theme-import-*")
	if err != nil {
		return ImportThemeResult{ErrorMsg: "创建临时目录失败"}
	}
	defer os.RemoveAll(tmpDir)

	tmpFile := filepath.Join(tmpDir, "theme.yaml")
	if err := os.WriteFile(tmpFile, content, 0o644); err != nil {
		return ImportThemeResult{ErrorMsg: "写入临时文件失败"}
	}

	mgr := theme.NewLightweightManager(nil)
	if err := mgr.LoadTheme(tmpFile); err != nil {
		return ImportThemeResult{ErrorMsg: "主题校验失败: " + err.Error()}
	}
	if mgr.GetResolvedV3() == nil {
		return ImportThemeResult{ErrorMsg: "主题非 v3 格式或解析失败（缺少有效的 colors 块）"}
	}

	// 4. 按 meta.name 检测同名冲突（目录名可能与 slug 不同，必须扫描内容）
	existingDir := findUserThemeDirByName(userThemesDir, t.Meta.Name)
	if existingDir != "" && !force {
		return ImportThemeResult{
			ThemeName: t.Meta.Name,
			Conflict:  true,
			ErrorMsg:  fmt.Sprintf("已存在主题「%s」", t.Meta.Name),
		}
	}

	// 5. 确定目标目录：有同名主题则原地覆盖，否则按 slug 新建
	destDir := existingDir
	if destDir == "" {
		destDir = filepath.Join(userThemesDir, sanitizeThemeSlug(t.Meta.Name))
	}

	if err := os.MkdirAll(destDir, 0o755); err != nil {
		return ImportThemeResult{ErrorMsg: "创建主题目录失败: " + err.Error()}
	}
	if err := os.WriteFile(filepath.Join(destDir, "theme.yaml"), content, 0o644); err != nil {
		return ImportThemeResult{ErrorMsg: "写入主题文件失败: " + err.Error()}
	}

	return ImportThemeResult{
		Success:   true,
		ThemeName: t.Meta.Name,
	}
}

// findUserThemeDirByName 扫描 userThemesDir 下所有子目录，返回第一个 meta.name 匹配的绝对路径；
// 未找到则返回空字符串。
func findUserThemeDirByName(userThemesDir, name string) string {
	entries, err := os.ReadDir(userThemesDir)
	if err != nil {
		return ""
	}
	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}
		themeFile := filepath.Join(userThemesDir, entry.Name(), "theme.yaml")
		data, err := os.ReadFile(themeFile)
		if err != nil {
			continue
		}
		var t theme.Theme
		if err := yaml.Unmarshal(data, &t); err != nil {
			continue
		}
		if t.Meta.Name == name {
			return filepath.Join(userThemesDir, entry.Name())
		}
	}
	return ""
}

// DeleteTheme 删除用户安装的主题目录（内置主题不可删除）。
// themeName 为主题 ID（即目录名），与 ThemeInfo.Name 对应。
func (a *App) DeleteTheme(themeName string) error {
	if theme.BuiltinThemeIDs[themeName] {
		return fmt.Errorf("内置主题不可删除")
	}
	userThemesDir, err := config.GetThemesUserDir()
	if err != nil {
		return fmt.Errorf("获取用户主题目录失败: %w", err)
	}
	themeDir := filepath.Join(userThemesDir, themeName)
	// 路径遮越安全检查
	rel, err := filepath.Rel(userThemesDir, themeDir)
	if err != nil || strings.HasPrefix(rel, "..") {
		return fmt.Errorf("非法主题路径")
	}
	if _, err := os.Stat(themeDir); os.IsNotExist(err) {
		return fmt.Errorf("主题不存在: %s", themeName)
	}
	wailsRuntime.LogInfof(a.ctx, "[setting] 删除主题 id=%s", themeName)
	return os.RemoveAll(themeDir)
}

// OpenThemesFolder 在系统文件管理器中打开用户主题目录。
func (a *App) OpenThemesFolder() error {
	userThemesDir, err := config.GetThemesUserDir()
	if err != nil {
		return fmt.Errorf("获取用户主题目录失败: %w", err)
	}
	if err := os.MkdirAll(userThemesDir, 0o755); err != nil {
		return fmt.Errorf("创建主题目录失败: %w", err)
	}
	wailsRuntime.LogInfof(a.ctx, "[setting] 打开主题目录 len=%d", len(userThemesDir))
	return shellOpen(userThemesDir)
}

// sanitizeThemeSlug 将 meta.name 转为合法的 Windows 目录名：
// 去除非法字符（\ / : * ? " < > |），空格替换为下划线，保留其余字符。
func sanitizeThemeSlug(name string) string {
	const illegal = `\/:*?"<>|`
	var sb strings.Builder
	for _, r := range name {
		if strings.ContainsRune(illegal, r) {
			continue
		}
		if r == ' ' {
			sb.WriteRune('_')
		} else {
			sb.WriteRune(r)
		}
	}
	result := strings.TrimSpace(sb.String())
	if result == "" {
		return "imported_theme"
	}
	return result
}
