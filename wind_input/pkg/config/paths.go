// Package config 提供配置管理的公共功能
package config

import (
	"fmt"
	"os"
	"path/filepath"
)

const (
	AppName           = "WindInput"
	DataSubDir        = "data"                // 程序目录下的数据子目录
	ConfigFileName    = "config.yaml"         // 用户配置
	StateFileName     = "state.yaml"          // 用户状态
	SystemPhrasesFile = "system.phrases.yaml" // 系统短语（data/ 目录）
	UserPhrasesFile   = "user.phrases.yaml"   // 用户短语（用户目录）
)

// GetConfigDir returns the user configuration directory path
// On Windows: %APPDATA%\WindInput
func GetConfigDir() (string, error) {
	configDir, err := os.UserConfigDir()
	if err != nil {
		return "", fmt.Errorf("failed to get user config dir: %w", err)
	}
	return filepath.Join(configDir, AppName), nil
}

// GetDataDir returns the program data directory path (exeDir/data)
func GetDataDir(exeDir string) string {
	return filepath.Join(exeDir, DataSubDir)
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

// GetUserPhrasesPath returns the full path to the user phrases file
func GetUserPhrasesPath() (string, error) {
	configDir, err := GetConfigDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(configDir, UserPhrasesFile), nil
}

// EnsureConfigDir ensures the config directory exists
func EnsureConfigDir() error {
	configDir, err := GetConfigDir()
	if err != nil {
		return err
	}
	return os.MkdirAll(configDir, 0755)
}
