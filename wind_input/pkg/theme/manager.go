package theme

import (
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"sync"

	"gopkg.in/yaml.v3"
)

// Manager manages theme loading and switching
type Manager struct {
	logger         *slog.Logger
	mu             sync.RWMutex
	currentTheme   *Theme
	currentThemeID string // Theme ID used for loading (e.g., "default", "dark")
	resolved       *ResolvedTheme
	themeDirs      []string // Directories to search for themes
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
		m.resolved = m.currentTheme.Resolve()
	}

	return m
}

// initThemeDirs initializes the theme search directories
func (m *Manager) initThemeDirs() {
	m.themeDirs = []string{}

	// 1. User themes directory: %APPDATA%\WindInput\themes
	if appData := os.Getenv("APPDATA"); appData != "" {
		userThemesDir := filepath.Join(appData, "WindInput", "themes")
		m.themeDirs = append(m.themeDirs, userThemesDir)
	}

	// 2. Executable directory: <exe_dir>/themes
	if exe, err := os.Executable(); err == nil {
		exeDir := filepath.Dir(exe)
		themesDir := filepath.Join(exeDir, "themes")
		m.themeDirs = append(m.themeDirs, themesDir)
	}

	if m.logger != nil {
		m.logger.Debug("Theme search directories initialized", "dirs", m.themeDirs)
	}
}

// loadAndApply loads a theme from file and applies it (caller must not hold lock)
func (m *Manager) loadAndApply(name string) error {
	theme, err := m.loadThemeFile(name)
	if err != nil {
		return err
	}
	m.currentTheme = theme
	m.currentThemeID = name
	m.resolved = m.currentTheme.Resolve()
	return nil
}

// LoadTheme loads a theme by name from theme directories.
// Name can be:
// - A theme directory name to search in theme directories (e.g., "default", "dark", "msime")
// - An absolute path to a theme.yaml file
func (m *Manager) LoadTheme(name string) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	if name == "" {
		name = "default"
	}

	// Try to load from file
	theme, err := m.loadThemeFile(name)
	if err != nil {
		if m.logger != nil {
			m.logger.Error("加载主题失败", "name", name, "error", err,
				"search_dirs", m.themeDirs)
		}
		return fmt.Errorf("加载主题 %q 失败: %w (搜索路径: %v)", name, err, m.themeDirs)
	}

	m.currentTheme = theme
	m.currentThemeID = name
	m.resolved = m.currentTheme.Resolve()
	if m.logger != nil {
		m.logger.Info("Loaded theme", "name", theme.Meta.Name, "id", name)
	}
	return nil
}

// loadThemeFile attempts to load a theme from various locations
func (m *Manager) loadThemeFile(name string) (*Theme, error) {
	// If it's an absolute path to a file, load directly
	if filepath.IsAbs(name) {
		return m.loadThemeFromPath(name)
	}

	// Search in theme directories
	for _, dir := range m.themeDirs {
		// Try <dir>/<name>/theme.yaml
		themePath := filepath.Join(dir, name, "theme.yaml")
		if _, err := os.Stat(themePath); err == nil {
			return m.loadThemeFromPath(themePath)
		}

		// Try <dir>/<name>.yaml
		themePath = filepath.Join(dir, name+".yaml")
		if _, err := os.Stat(themePath); err == nil {
			return m.loadThemeFromPath(themePath)
		}
	}

	return nil, fmt.Errorf("theme not found: %s", name)
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

// GetResolvedTheme returns the resolved (parsed) theme
func (m *Manager) GetResolvedTheme() *ResolvedTheme {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.resolved
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
	ID          string // Theme ID used for loading (e.g., "default", "dark")
	DisplayName string // Human-readable name (e.g., "默认主题 1.0")
}

// ListAvailableThemeInfos returns theme display info (ID + display name) for all available themes
func (m *Manager) ListAvailableThemeInfos() []ThemeDisplayInfo {
	ids := m.ListAvailableThemes()
	infos := make([]ThemeDisplayInfo, 0, len(ids))

	for _, id := range ids {
		displayName := id
		// Try to read display name from theme file
		if t, err := m.loadThemeFile(id); err == nil && t.Meta.Name != "" {
			displayName = t.Meta.Name
			if t.Meta.Version != "" {
				displayName += " " + t.Meta.Version
			}
		}
		infos = append(infos, ThemeDisplayInfo{ID: id, DisplayName: displayName})
	}

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
