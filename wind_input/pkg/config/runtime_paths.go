package config

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/huanfeng/wind_input/pkg/buildvariant"
)

const (
	// PortableMarkerName 便携模式标记文件名（wind_portable 和 wind_input 共用）
	PortableMarkerName = "wind_portable_mode"
	// PortableDataDir 便携模式下用户数据目录名
	PortableDataDir = "userdata"
)

// findPortableRoot checks whether the portable marker exists in exeDir.
// The marker must be in the same directory as the executable, not in parent directories,
// to avoid false positives when multiple portable instances are nested.
func findPortableRoot(exeDir string) (string, bool) {
	dir := filepath.Clean(exeDir)
	if _, err := os.Stat(filepath.Join(dir, PortableMarkerName)); err == nil {
		return dir, true
	}
	return "", false
}

// ResolveUserDataDir returns the application user data directory based on the
// runtime mode.
// Priority: portable marker > datadir.conf > default (%APPDATA%\WindInput)
func ResolveUserDataDir() (string, error) {
	exeDir, err := GetExeDir()
	if err == nil {
		if root, ok := findPortableRoot(exeDir); ok {
			return filepath.Join(root, PortableDataDir), nil
		}
	}

	// 读取 datadir.conf 自定义路径
	override, err := ReadUserDataDirOverride()
	if err == nil && override != "" {
		return override, nil
	}

	// 默认使用 %APPDATA%\WindInput
	configDir, err := os.UserConfigDir()
	if err != nil {
		return "", fmt.Errorf("failed to get user config dir: %w", err)
	}
	return filepath.Join(configDir, buildvariant.AppName()), nil
}

// ResolveLocalDataDir returns the local writable data directory. Portable mode
// shares the same user data root so logs/cache stay movable together.
func ResolveLocalDataDir() (string, error) {
	exeDir, err := GetExeDir()
	if err == nil {
		if root, ok := findPortableRoot(exeDir); ok {
			return filepath.Join(root, PortableDataDir), nil
		}
	}

	if cacheDir, err := os.UserCacheDir(); err == nil {
		return filepath.Join(cacheDir, buildvariant.AppName()), nil
	}
	return "", fmt.Errorf("failed to resolve local data dir")
}

func GetLogsDir() (string, error) {
	base, err := ResolveLocalDataDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(base, "logs"), nil
}

func GetCacheDir() (string, error) {
	base, err := ResolveLocalDataDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(base, "cache"), nil
}

func GetThemesUserDir() (string, error) {
	base, err := ResolveUserDataDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(base, "themes"), nil
}

// IsPortableMode returns whether the application is running in portable mode.
func IsPortableMode() bool {
	exeDir, err := GetExeDir()
	if err != nil {
		return false
	}
	_, ok := findPortableRoot(exeDir)
	return ok
}

// GetConfigDirDisplay returns a user-friendly display string for the config directory.
func GetConfigDirDisplay() string {
	exeDir, err := GetExeDir()
	if err == nil {
		if root, ok := findPortableRoot(exeDir); ok {
			rel, err := filepath.Rel(root, filepath.Join(root, PortableDataDir))
			if err == nil {
				return `<安装目录>\` + rel
			}
		}
	}

	// 检查是否有自定义路径
	override, err := ReadUserDataDirOverride()
	if err == nil && override != "" {
		return override
	}

	return `%APPDATA%\` + buildvariant.AppName()
}

// GetLogsDirDisplay returns a user-friendly display string for the logs directory.
// Standard mode: %LOCALAPPDATA%\WindInput\logs; Portable mode: <安装目录>\userdata\logs
func GetLogsDirDisplay() string {
	exeDir, err := GetExeDir()
	if err == nil {
		if root, ok := findPortableRoot(exeDir); ok {
			rel, err := filepath.Rel(root, filepath.Join(root, PortableDataDir, "logs"))
			if err == nil {
				return `<安装目录>\` + rel
			}
		}
	}
	return `%LOCALAPPDATA%\` + buildvariant.AppName() + `\logs`
}
