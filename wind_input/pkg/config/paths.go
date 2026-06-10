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
	ConfigFileName    = "config.toml"         // 用户配置
	StateFileName     = "state.toml"          // 用户状态
	SystemPhrasesFile = "system.phrases.yaml" // 系统短语（data/ 目录 和 用户目录同名覆盖）
	SystemConfigFile  = "config.toml"         // 系统预置配置（data/ 目录）

	// 旧版 YAML 文件名：双读回退用。用户目录下的旧文件在首次加载时
	// 自动迁移为 TOML 并改名 *.migrated.bak（见 codec.go）。
	LegacyConfigFileName       = "config.yaml"
	LegacyStateFileName        = "state.yaml"
	LegacySystemConfigFileName = "config.yaml"
)

// GetConfigDir returns the user configuration directory path.
// Standard mode: %APPDATA%\WindInput (or custom via datadir.conf)
// Portable mode: <exe_dir>\userdata
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

// GetSystemConfigPath returns the path to the system default config file
// (data/config.toml). 若 TOML 不存在但旧版 data/config.yaml 存在（如升级
// 安装后残留的旧文件、或旧版安装包），回退到旧版路径。
func GetSystemConfigPath() (string, error) {
	exeDir, err := GetExeDir()
	if err != nil {
		return "", err
	}
	dataDir := GetDataDir(exeDir)
	tomlPath := filepath.Join(dataDir, SystemConfigFile)
	if _, err := os.Stat(tomlPath); err == nil {
		return tomlPath, nil
	}
	legacyPath := filepath.Join(dataDir, LegacySystemConfigFileName)
	if _, err := os.Stat(legacyPath); err == nil {
		return legacyPath, nil
	}
	return tomlPath, nil
}

// GetOpenCCDir returns the directory path where OpenCC .octrie dictionaries live (data/opencc).
func GetOpenCCDir() (string, error) {
	exeDir, err := GetExeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(GetDataDir(exeDir), "opencc"), nil
}

// EnsureConfigDir ensures the config directory exists
func EnsureConfigDir() error {
	configDir, err := GetConfigDir()
	if err != nil {
		return err
	}
	return os.MkdirAll(configDir, 0755)
}
