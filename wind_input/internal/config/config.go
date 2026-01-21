// Package config handles application configuration
package config

import (
	"fmt"
	"os"
	"path/filepath"

	"gopkg.in/yaml.v3"
)

const (
	AppName        = "WindInput"
	ConfigFileName = "config.yaml"
	UserDictFile   = "user_dict.txt"
)

// Config represents the application configuration
type Config struct {
	General    GeneralConfig    `yaml:"general"`
	Dictionary DictionaryConfig `yaml:"dictionary"`
	Hotkeys    HotkeyConfig     `yaml:"hotkeys"`
	UI         UIConfig         `yaml:"ui"`
}

// GeneralConfig contains general settings
type GeneralConfig struct {
	StartInChineseMode bool   `yaml:"start_in_chinese_mode"`
	LogLevel           string `yaml:"log_level"`
}

// DictionaryConfig contains dictionary settings
type DictionaryConfig struct {
	SystemDict string `yaml:"system_dict"`
	UserDict   string `yaml:"user_dict"`
}

// HotkeyConfig contains hotkey settings
type HotkeyConfig struct {
	ToggleMode string `yaml:"toggle_mode"` // "shift", "ctrl+space", etc.
}

// UIConfig contains UI settings
type UIConfig struct {
	FontSize          float64 `yaml:"font_size"`
	CandidatesPerPage int     `yaml:"candidates_per_page"`
	FontPath          string  `yaml:"font_path"`
}

// DefaultConfig returns the default configuration
func DefaultConfig() *Config {
	return &Config{
		General: GeneralConfig{
			StartInChineseMode: true,
			LogLevel:           "info",
		},
		Dictionary: DictionaryConfig{
			SystemDict: "dict/pinyin/base.txt",
			UserDict:   UserDictFile,
		},
		Hotkeys: HotkeyConfig{
			ToggleMode: "shift",
		},
		UI: UIConfig{
			FontSize:          18,
			CandidatesPerPage: 9,
			FontPath:          "",
		},
	}
}

// GetConfigDir returns the configuration directory path
// On Windows: %APPDATA%\WindInput
func GetConfigDir() (string, error) {
	configDir, err := os.UserConfigDir()
	if err != nil {
		return "", fmt.Errorf("failed to get user config dir: %w", err)
	}
	return filepath.Join(configDir, AppName), nil
}

// GetConfigPath returns the full path to the config file
func GetConfigPath() (string, error) {
	configDir, err := GetConfigDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(configDir, ConfigFileName), nil
}

// GetUserDictPath returns the full path to the user dictionary
func GetUserDictPath() (string, error) {
	configDir, err := GetConfigDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(configDir, UserDictFile), nil
}

// EnsureConfigDir ensures the config directory exists
func EnsureConfigDir() error {
	configDir, err := GetConfigDir()
	if err != nil {
		return err
	}
	return os.MkdirAll(configDir, 0755)
}

// Load loads the configuration from file
// If the file doesn't exist, returns default configuration
func Load() (*Config, error) {
	configPath, err := GetConfigPath()
	if err != nil {
		return DefaultConfig(), err
	}

	data, err := os.ReadFile(configPath)
	if err != nil {
		if os.IsNotExist(err) {
			// Return default config if file doesn't exist
			return DefaultConfig(), nil
		}
		return DefaultConfig(), fmt.Errorf("failed to read config file: %w", err)
	}

	config := DefaultConfig()
	if err := yaml.Unmarshal(data, config); err != nil {
		return DefaultConfig(), fmt.Errorf("failed to parse config file: %w", err)
	}

	return config, nil
}

// Save saves the configuration to file
func Save(config *Config) error {
	if err := EnsureConfigDir(); err != nil {
		return fmt.Errorf("failed to create config dir: %w", err)
	}

	configPath, err := GetConfigPath()
	if err != nil {
		return err
	}

	data, err := yaml.Marshal(config)
	if err != nil {
		return fmt.Errorf("failed to marshal config: %w", err)
	}

	if err := os.WriteFile(configPath, data, 0644); err != nil {
		return fmt.Errorf("failed to write config file: %w", err)
	}

	return nil
}

// SaveDefault saves the default configuration to file
func SaveDefault() error {
	return Save(DefaultConfig())
}
