// Package config 提供配置管理的公共功能
package config

import (
	"fmt"
	"os"
	"path/filepath"
)

const (
	AppName        = "WindInput"
	ConfigFileName = "config.yaml"
	StateFileName  = "state.yaml"
	UserDictFile   = "user_dict.txt"
	PhrasesFile    = "phrases.yaml"
	ShadowFile     = "shadow.yaml"
)

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

// GetStatePath returns the full path to the state file
func GetStatePath() (string, error) {
	configDir, err := GetConfigDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(configDir, StateFileName), nil
}

// GetUserDictPath returns the full path to the user dictionary
func GetUserDictPath() (string, error) {
	configDir, err := GetConfigDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(configDir, UserDictFile), nil
}

// GetPhrasesPath returns the full path to the phrases file
func GetPhrasesPath() (string, error) {
	configDir, err := GetConfigDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(configDir, PhrasesFile), nil
}

// GetShadowPath returns the full path to the shadow file
func GetShadowPath() (string, error) {
	configDir, err := GetConfigDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(configDir, ShadowFile), nil
}

// EnsureConfigDir ensures the config directory exists
func EnsureConfigDir() error {
	configDir, err := GetConfigDir()
	if err != nil {
		return err
	}
	return os.MkdirAll(configDir, 0755)
}

// GetWubiDictPath returns the path to the wubi dictionary
func GetWubiDictPath() string {
	return "dict/wubi/wubi86.txt"
}

// GetPinyinDictPath returns the path to the pinyin dictionary
func GetPinyinDictPath() string {
	return "dict/pinyin/pinyin.txt"
}
