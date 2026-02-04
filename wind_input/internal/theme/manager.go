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
	logger       *slog.Logger
	mu           sync.RWMutex
	currentTheme *Theme
	resolved     *ResolvedTheme
	themeDirs    []string // Directories to search for themes
}

// NewManager creates a new theme manager
func NewManager(logger *slog.Logger) *Manager {
	m := &Manager{
		logger:       logger,
		currentTheme: DefaultTheme(),
	}
	m.resolved = m.currentTheme.Resolve()

	// Initialize theme search paths
	m.initThemeDirs()

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

// LoadTheme loads a theme by name
// Name can be:
// - "default" or "dark" for built-in themes
// - A theme directory name to search in theme directories
// - An absolute path to a theme.yaml file
func (m *Manager) LoadTheme(name string) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	// Handle built-in themes
	if name == "default" || name == "" {
		m.currentTheme = DefaultTheme()
		m.resolved = m.currentTheme.Resolve()
		if m.logger != nil {
			m.logger.Info("Loaded built-in default theme")
		}
		return nil
	}

	if name == "dark" {
		m.currentTheme = DarkTheme()
		m.resolved = m.currentTheme.Resolve()
		if m.logger != nil {
			m.logger.Info("Loaded built-in dark theme")
		}
		return nil
	}

	// Try to load from file
	theme, err := m.loadThemeFile(name)
	if err != nil {
		if m.logger != nil {
			m.logger.Warn("Failed to load theme, using default", "name", name, "error", err)
		}
		// Fall back to default theme
		m.currentTheme = DefaultTheme()
		m.resolved = m.currentTheme.Resolve()
		return err
	}

	m.currentTheme = theme
	m.resolved = m.currentTheme.Resolve()
	if m.logger != nil {
		m.logger.Info("Loaded theme", "name", theme.Meta.Name, "path", name)
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

	// Start with default theme to fill in any missing values
	theme := DefaultTheme()
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
	themes := []string{"default", "dark"}

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
					themes = append(themes, entry.Name())
				}
			} else if filepath.Ext(entry.Name()) == ".yaml" {
				// Single file theme
				themeName := entry.Name()[:len(entry.Name())-5] // Remove .yaml
				themes = append(themes, themeName)
			}
		}
	}

	// Deduplicate
	seen := make(map[string]bool)
	result := make([]string, 0, len(themes))
	for _, t := range themes {
		if !seen[t] {
			seen[t] = true
			result = append(result, t)
		}
	}

	return result
}

// GetThemeDirs returns the theme search directories
func (m *Manager) GetThemeDirs() []string {
	return m.themeDirs
}
