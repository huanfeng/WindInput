package config

import (
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"

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
	// macOS 例外：遵循系统惯例把日志放 ~/Library/Logs/<App>
	// （Console.app 可聚合查看，且不会被 ~/Library/Caches 的清理策略回收）。
	// Portable 模式仍随便携数据目录走，保持可移动。
	if runtime.GOOS == "darwin" {
		if exeDir, err := GetExeDir(); err == nil {
			if root, ok := findPortableRoot(exeDir); ok {
				return filepath.Join(root, PortableDataDir, "logs"), nil
			}
		}
		home, err := os.UserHomeDir()
		if err != nil || home == "" {
			return "", fmt.Errorf("failed to resolve home dir for logs: %w", err)
		}
		return filepath.Join(home, "Library", "Logs", buildvariant.AppName()), nil
	}

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

func GetScreenshotsDir() (string, error) {
	base, err := ResolveUserDataDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(base, "screenshots"), nil
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
		return abbreviateHome(override)
	}

	// 默认目录显示串：Windows 用 %APPDATA% 占位串；其余平台（macOS/Linux）
	// 显示真实解析路径（home 缩写为 ~），如 ~/Library/Application Support/WindInput。
	if runtime.GOOS == "windows" {
		return `%APPDATA%\` + buildvariant.AppName()
	}
	if dir, err := ResolveUserDataDir(); err == nil {
		return abbreviateHome(dir)
	}
	return filepath.Join("~", buildvariant.AppName())
}

// abbreviateHome 把路径中的用户主目录前缀替换为 ~，便于跨平台友好显示。
// 非主目录下的路径原样返回。
func abbreviateHome(p string) string {
	home, err := os.UserHomeDir()
	if err != nil || home == "" {
		return p
	}
	rel, err := filepath.Rel(home, p)
	if err != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
		return p
	}
	if rel == "." {
		return "~"
	}
	return filepath.Join("~", rel)
}

// GetLogsDirDisplay returns a user-friendly display string for the logs directory.
// Standard mode (Windows): %LOCALAPPDATA%\WindInput\logs; Portable mode: <安装目录>\userdata\logs;
// macOS: 真实解析路径（home 缩写为 ~），即 ~/Library/Logs/WindInput。
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
	if runtime.GOOS == "windows" {
		return `%LOCALAPPDATA%\` + buildvariant.AppName() + `\logs`
	}
	if dir, err := GetLogsDir(); err == nil {
		return abbreviateHome(dir)
	}
	return filepath.Join("~", buildvariant.AppName(), "logs")
}
