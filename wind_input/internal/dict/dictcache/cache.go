// Package dictcache 提供词库缓存管理功能
// 负责将文本格式词库转换为 wdb 二进制格式并缓存到本地
package dictcache

import (
	"os"
	"path/filepath"

	"github.com/huanfeng/wind_input/pkg/buildvariant"
	"github.com/huanfeng/wind_input/pkg/config"
)

var cacheDir string

// GetCacheDir 返回缓存目录路径，不存在则创建
func GetCacheDir() string {
	if cacheDir != "" {
		return cacheDir
	}
	resolved, err := config.GetCacheDir()
	if err != nil {
		cacheDir = filepath.Join(os.TempDir(), buildvariant.AppName(), "cache")
	} else {
		cacheDir = resolved
	}
	os.MkdirAll(cacheDir, 0755)
	return cacheDir
}

// CachePath 返回缓存文件的完整路径
func CachePath(name string) string {
	return filepath.Join(GetCacheDir(), name+".wdb")
}

// WdatCachePath 返回 wdat 缓存文件的完整路径
func WdatCachePath(name string) string {
	return filepath.Join(GetCacheDir(), name+".wdat")
}

// NeedsRegenerate 判断是否需要重新生成 wdb 缓存
// 当 wdb 不存在或任一源文件 mtime > wdb mtime 时返回 true
func NeedsRegenerate(srcPaths []string, wdbPath string) bool {
	wdbInfo, err := os.Stat(wdbPath)
	if err != nil {
		return true
	}
	wdbMtime := wdbInfo.ModTime()

	for _, src := range srcPaths {
		srcInfo, err := os.Stat(src)
		if err != nil {
			continue
		}
		if srcInfo.ModTime().After(wdbMtime) {
			return true
		}
	}
	return false
}
