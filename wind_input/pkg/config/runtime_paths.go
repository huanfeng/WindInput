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
	// maxPortableDepth 向上遍历最大层数（从 exeDir 开始，最多再向上 2 层）
	maxPortableDepth = 2
)

// findPortableRoot walks upward from exeDir looking for the portable marker.
// It checks at most maxPortableDepth parent directories above exeDir.
func findPortableRoot(exeDir string) (string, bool) {
	dir := filepath.Clean(exeDir)
	for i := 0; i <= maxPortableDepth; i++ {
		if _, err := os.Stat(filepath.Join(dir, PortableMarkerName)); err == nil {
			return dir, true
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			return "", false
		}
		dir = parent
	}
	return "", false
}

// ResolveUserDataDir returns the application user data directory based on the
// runtime mode. Portable mode uses <portableRoot>/userdata directly (no AppName subdir).
func ResolveUserDataDir() (string, error) {
	exeDir, err := GetExeDir()
	if err == nil {
		if root, ok := findPortableRoot(exeDir); ok {
			return filepath.Join(root, PortableDataDir), nil
		}
	}

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
