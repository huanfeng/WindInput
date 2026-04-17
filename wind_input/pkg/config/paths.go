// Package config 提供配置管理的公共功能
package config

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/huanfeng/wind_input/pkg/buildvariant"
)

const (
	DataSubDir        = "data"                // 程序目录下的数据子目录
	ConfigFileName    = "config.yaml"         // 用户配置
	StateFileName     = "state.yaml"          // 用户状态
	SystemPhrasesFile = "system.phrases.yaml" // 系统短语（data/ 目录 和 用户目录同名覆盖）
	UserPhrasesFile   = "user.phrases.yaml"   // 用户短语（用户目录）
	SystemConfigFile  = "config.yaml"         // 系统预置配置（data/ 目录）
)

// GetConfigDir returns the user configuration directory path
// On Windows: %APPDATA%\WindInput
func GetConfigDir() (string, error) {
	return ResolveUserDataDir()
}

// GetAppName returns the application name based on build variant
func GetAppName() string {
	return buildvariant.AppName()
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

// GetSystemPhrasesUserPath returns the full path to the user's system phrases override file
// (same filename as system, but in user config directory)
func GetSystemPhrasesUserPath() (string, error) {
	configDir, err := GetConfigDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(configDir, SystemPhrasesFile), nil
}

// GetExeDir returns the directory containing the current executable
func GetExeDir() (string, error) {
	exePath, err := os.Executable()
	if err != nil {
		return "", fmt.Errorf("failed to get executable path: %w", err)
	}
	return filepath.Dir(exePath), nil
}

// GetSystemConfigPath returns the path to the system default config file (data/config.yaml)
func GetSystemConfigPath() (string, error) {
	exeDir, err := GetExeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(GetDataDir(exeDir), SystemConfigFile), nil
}

// EnsureConfigDir ensures the config directory exists
func EnsureConfigDir() error {
	configDir, err := GetConfigDir()
	if err != nil {
		return err
	}
	return os.MkdirAll(configDir, 0755)
}
