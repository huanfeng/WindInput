package theme

import (
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"

	"github.com/huanfeng/wind_input/pkg/config"
	"gopkg.in/yaml.v3"
)

// BuiltinThemeIDs lists theme IDs that are considered built-in (not third-party).
// Third-party themes get their sort order +100 to keep built-in themes first.
var BuiltinThemeIDs = map[string]bool{
	"default": true,
	"msime":   true,
}

// Manager manages theme loading and switching
type Manager struct {
	logger          *slog.Logger
	mu              sync.RWMutex
	currentTheme    *Theme
	currentThemeID  string       // Theme ID used for loading (e.g., "default", "msime")
	currentThemeDir string       // theme.yaml 所在目录（v2.5 用于定位 _layouts/_palettes 与背景图）
	resolvedV25     *ResolvedV25 // v2.5 解析结果（P5：渲染层统一消费此结构；adapter/ResolvedTheme 已退役）
	isDarkMode      bool         // Current dark mode state
	themeDirs       []string     // Directories to search for themes
}

// NewLightweightManager 仅初始化搜索路径，不预加载 default 主题。
// 适用于 preview / 临时查询场景，避免 NewManager 的双重 resolve 开销。
func NewLightweightManager(logger *slog.Logger) *Manager {
	m := &Manager{logger: logger}
	m.initThemeDirs()
	return m
}

// NewManager creates a new theme manager
func NewManager(logger *slog.Logger) *Manager {
	m := &Manager{
		logger: logger,
	}

	// Initialize theme search paths
	m.initThemeDirs()

	// Try to load "default" theme from file
	if err := m.loadAndApply("default"); err != nil {
		if logger != nil {
			logger.Warn("无法从文件加载默认主题，使用内置空主题", "error", err)
		}
		m.currentTheme = emptyTheme()
		m.currentThemeID = "default"
		// emptyTheme 非 v2.5，resolvedV25 保持 nil（渲染层各窗口对 nil 用内置默认色）
	}

	return m
}

// initThemeDirs initializes the theme search directories
func (m *Manager) initThemeDirs() {
	m.themeDirs = []string{}

	// 1. User themes directory
	if userThemesDir, err := config.GetThemesUserDir(); err == nil {
		m.themeDirs = append(m.themeDirs, userThemesDir)
	}

	// 2. Program data directory: <exe_dir>/data/themes
	if exe, err := os.Executable(); err == nil {
		exeDir := filepath.Dir(exe)
		themesDir := filepath.Join(exeDir, "data", "themes")
		m.themeDirs = append(m.themeDirs, themesDir)
	}

	if m.logger != nil {
		m.logger.Debug("Theme search directories initialized", "dirs", m.themeDirs)
	}
}

// loadAndApply loads a theme from file and applies it (caller must not hold lock)
func (m *Manager) loadAndApply(name string) error {
	theme, themeDir, err := m.loadThemeFileWithDir(name)
	if err != nil {
		return err
	}
	m.currentTheme = theme
	m.currentThemeID = name
	m.currentThemeDir = themeDir
	m.resolvedV25 = m.resolveTheme(theme, themeDir)
	return nil
}

// resolveTheme 解析主题为 ResolvedV25（仅支持 v2.5；非 v2.5 或解析失败返回 nil）。
// P5：adapter/ResolvedTheme/v2 路径已退役。
func (m *Manager) resolveTheme(t *Theme, themeDir string) *ResolvedV25 {
	return m.resolveThemeWithDark(t, themeDir, m.isDarkMode)
}

// resolveThemeWithDark 与 resolveTheme 等价，但显式接收 isDark 参数，
// 用于在锁外执行解析时携带快照值。
func (m *Manager) resolveThemeWithDark(t *Theme, themeDir string, isDark bool) *ResolvedV25 {
	if !t.HasV25Schema() {
		if m.logger != nil {
			m.logger.Warn("主题非 v2.5 格式（已放弃 v2/legacy 支持），无法解析", "name", t.Meta.Name)
		}
		return nil
	}
	rv, err := m.ResolveV25(t, isDark, themeDir)
	if err != nil {
		if m.logger != nil {
			m.logger.Warn("v2.5 主题解析失败", "error", err)
		}
		return nil
	}
	return rv
}

// LoadTheme loads a theme by name from theme directories.
// Name can be:
// - A theme directory name to search in theme directories (e.g., "default", "msime")
// - An absolute path to a theme.yaml file
func (m *Manager) LoadTheme(name string) error {
	if name == "" {
		name = "default"
	}

	// 1) 锁外做磁盘 I/O 与解析（避免持锁 10ms+ 与 coordinator slow request 冲突）
	m.mu.RLock()
	isDark := m.isDarkMode
	m.mu.RUnlock()

	theme, themeDir, err := m.loadThemeFileWithDir(name)
	if err != nil {
		if m.logger != nil {
			m.logger.Error("加载主题失败", "name", name, "error", err,
				"search_dirs", m.themeDirs)
		}
		return fmt.Errorf("加载主题 %q 失败: %w (搜索路径: %v)", name, err, m.themeDirs)
	}
	rv := m.resolveThemeWithDark(theme, themeDir, isDark)

	// 2) 仅在 commit 字段时持锁
	m.mu.Lock()
	m.currentTheme = theme
	m.currentThemeID = name
	m.currentThemeDir = themeDir
	m.resolvedV25 = rv
	m.mu.Unlock()

	if m.logger != nil {
		m.logger.Info("Loaded theme", "name", theme.Meta.Name, "id", name, "isDark", isDark, "v25", theme.HasV25Schema())
	}
	return nil
}

// SetDarkMode updates the dark mode state and re-resolves the current theme.
// Returns true if the mode actually changed.
func (m *Manager) SetDarkMode(isDark bool) bool {
	m.mu.Lock()
	defer m.mu.Unlock()

	if m.isDarkMode == isDark {
		return false
	}

	m.isDarkMode = isDark
	if m.currentTheme != nil {
		m.resolvedV25 = m.resolveTheme(m.currentTheme, m.currentThemeDir)
	}
	if m.logger != nil {
		m.logger.Info("Dark mode changed, theme re-resolved", "isDark", isDark, "theme", m.currentThemeID)
	}
	return true
}

// GetDarkMode returns the current dark mode state
func (m *Manager) GetDarkMode() bool {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.isDarkMode
}

// loadThemeFile attempts to load a theme from various locations
func (m *Manager) loadThemeFile(name string) (*Theme, error) {
	t, _, err := m.loadThemeFileWithDir(name)
	return t, err
}

// loadThemeFileWithDir 同 loadThemeFile，但额外返回 theme.yaml 所在目录，
// 用于 v2.5 解析时定位 _layouts/_palettes 与背景图相对路径。
func (m *Manager) loadThemeFileWithDir(name string) (*Theme, string, error) {
	if filepath.IsAbs(name) {
		t, err := m.loadThemeFromPath(name)
		return t, filepath.Dir(name), err
	}

	for _, dir := range m.themeDirs {
		themePath := filepath.Join(dir, name, "theme.yaml")
		if _, err := os.Stat(themePath); err == nil {
			t, err := m.loadThemeFromPath(themePath)
			return t, filepath.Dir(themePath), err
		}
		themePath = filepath.Join(dir, name+".yaml")
		if _, err := os.Stat(themePath); err == nil {
			t, err := m.loadThemeFromPath(themePath)
			return t, filepath.Dir(themePath), err
		}
	}
	return nil, "", fmt.Errorf("theme not found: %s", name)
}

// loadThemeFromPath loads a theme from a specific file path
func (m *Manager) loadThemeFromPath(path string) (*Theme, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("failed to read theme file: %w", err)
	}

	theme := &Theme{}
	if err := yaml.Unmarshal(data, theme); err != nil {
		return nil, fmt.Errorf("failed to parse theme file: %w", err)
	}

	return theme, nil
}

// GetCurrentTheme returns the current theme
func (m *Manager) GetCurrentTheme() *Theme {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.currentTheme
}

// GetResolvedV25 returns the v2.5 resolved theme (nil 表示主题非 v2.5 或解析失败).
// P5：adapter/ResolvedTheme 已退役，这是渲染层唯一的解析结果来源。
func (m *Manager) GetResolvedV25() *ResolvedV25 {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.resolvedV25
}

// ListAvailableThemes returns a list of available theme names
func (m *Manager) ListAvailableThemes() []string {
	seen := make(map[string]bool)
	var themes []string

	// Scan theme directories
	for _, dir := range m.themeDirs {
		entries, err := os.ReadDir(dir)
		if err != nil {
			continue
		}

		for _, entry := range entries {
			// 下划线前缀的目录与文件保留为 v2.5 共享零件库（_layouts / _palettes），不作为主题列出
			if strings.HasPrefix(entry.Name(), "_") {
				continue
			}
			if entry.IsDir() {
				// Check if it contains theme.yaml
				themePath := filepath.Join(dir, entry.Name(), "theme.yaml")
				if _, err := os.Stat(themePath); err == nil {
					name := entry.Name()
					if !seen[name] {
						seen[name] = true
						themes = append(themes, name)
					}
				}
			} else if filepath.Ext(entry.Name()) == ".yaml" {
				// Single file theme
				name := entry.Name()[:len(entry.Name())-5] // Remove .yaml
				if !seen[name] {
					seen[name] = true
					themes = append(themes, name)
				}
			}
		}
	}

	if len(themes) == 0 && m.logger != nil {
		m.logger.Warn("未找到任何主题文件", "search_dirs", m.themeDirs)
	}

	return themes
}

// ThemeDisplayInfo contains theme ID and display name
type ThemeDisplayInfo struct {
	ID          string // Theme ID used for loading (e.g., "default", "msime")
	DisplayName string // Human-readable name (e.g., "默认主题")
	Order       int    // Effective sort order (third-party themes get +100)
}

// ListAvailableThemeInfos returns theme display info sorted by order for all available themes.
// Third-party themes (not in BuiltinThemeIDs) get their order +100.
func (m *Manager) ListAvailableThemeInfos() []ThemeDisplayInfo {
	ids := m.ListAvailableThemes()
	infos := make([]ThemeDisplayInfo, 0, len(ids))

	for _, id := range ids {
		displayName := id
		order := 50 // default order for themes without explicit order
		// Try to read display name and order from theme file
		if t, err := m.loadThemeFile(id); err == nil {
			if t.Meta.Name != "" {
				displayName = t.Meta.Name
			}
			order = t.Meta.Order
		}

		// Third-party themes get +100 to their order
		if !BuiltinThemeIDs[id] {
			order += 100
		}

		infos = append(infos, ThemeDisplayInfo{ID: id, DisplayName: displayName, Order: order})
	}

	// Sort by order ascending, then by ID for stable ordering
	sort.Slice(infos, func(i, j int) bool {
		if infos[i].Order != infos[j].Order {
			return infos[i].Order < infos[j].Order
		}
		return infos[i].ID < infos[j].ID
	})

	return infos
}

// GetCurrentThemeID returns the ID of the currently loaded theme
func (m *Manager) GetCurrentThemeID() string {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.currentThemeID
}

// GetThemeDirs returns the theme search directories
func (m *Manager) GetThemeDirs() []string {
	return m.themeDirs
}
