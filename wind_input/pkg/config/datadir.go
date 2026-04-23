package config

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/huanfeng/wind_input/pkg/buildvariant"
)

const (
	// DataDirConfName 数据目录配置文件名
	DataDirConfName = "datadir.conf"
)

// dataDirConfPath 返回 datadir.conf 的完整路径
// 固定位于 %LOCALAPPDATA%\WindInput\datadir.conf
func dataDirConfPath() (string, error) {
	localDir, err := os.UserCacheDir()
	if err != nil {
		return "", fmt.Errorf("failed to get local app data dir: %w", err)
	}
	return filepath.Join(localDir, buildvariant.AppName(), DataDirConfName), nil
}

// readDataDirConf 读取 datadir.conf，返回其中的路径。
// 如果文件不存在或内容为空，返回空字符串。
func readDataDirConf(confPath string) (string, error) {
	data, err := os.ReadFile(confPath)
	if err != nil {
		if os.IsNotExist(err) {
			return "", nil
		}
		return "", err
	}
	p := strings.TrimSpace(string(data))
	return p, nil
}

// writeDataDirConf 将路径写入 datadir.conf
func writeDataDirConf(confPath string, dirPath string) error {
	dir := filepath.Dir(confPath)
	if err := os.MkdirAll(dir, 0755); err != nil {
		return fmt.Errorf("failed to create dir for datadir.conf: %w", err)
	}
	return os.WriteFile(confPath, []byte(dirPath), 0644)
}

// ReadUserDataDirOverride 读取用户自定义数据目录。
// 返回空字符串表示使用默认目录。
func ReadUserDataDirOverride() (string, error) {
	confPath, err := dataDirConfPath()
	if err != nil {
		return "", err
	}
	return readDataDirConf(confPath)
}

// WriteUserDataDirOverride 写入用户自定义数据目录。
// 传空字符串表示恢复默认。
func WriteUserDataDirOverride(dirPath string) error {
	confPath, err := dataDirConfPath()
	if err != nil {
		return err
	}
	return writeDataDirConf(confPath, dirPath)
}

// ValidateDataDirPath 验证数据目录路径合法性。
// 返回 (valid, warning)，warning 非空时表示有提示但仍合法。
func ValidateDataDirPath(path string) (bool, string) {
	if path == "" {
		return false, "路径不能为空"
	}

	// 必须是绝对路径
	if !filepath.IsAbs(path) {
		return false, "必须是绝对路径"
	}

	// 检查非法字符
	base := filepath.Clean(path)
	for _, c := range []byte{'*', '?', '"', '|', '<', '>'} {
		if strings.ContainsRune(base, rune(c)) {
			return false, fmt.Sprintf("路径包含非法字符: %c", c)
		}
	}

	// 检查是否为系统关键目录
	lower := strings.ToLower(base)
	systemPrefixes := []string{
		strings.ToLower(os.Getenv("WINDIR")),
		strings.ToLower(os.Getenv("PROGRAMFILES")),
		strings.ToLower(os.Getenv("PROGRAMFILES(X86)")),
		strings.ToLower(os.Getenv("SYSTEMROOT")),
	}
	for _, prefix := range systemPrefixes {
		if prefix != "" && (lower == prefix || strings.HasPrefix(lower, prefix+string(filepath.Separator))) {
			return false, "不能使用系统关键目录"
		}
	}

	return true, ""
}
