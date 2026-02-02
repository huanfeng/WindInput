// Package fileutil 提供文件操作的工具函数
package fileutil

import (
	"fmt"
	"os"
	"path/filepath"
	"time"
)

// FileState 文件状态，用于检测外部修改
type FileState struct {
	Path    string
	ModTime time.Time
	Size    int64
}

// GetFileState 获取文件状态
func GetFileState(path string) (*FileState, error) {
	info, err := os.Stat(path)
	if err != nil {
		if os.IsNotExist(err) {
			return &FileState{Path: path}, nil
		}
		return nil, fmt.Errorf("failed to stat file: %w", err)
	}

	return &FileState{
		Path:    path,
		ModTime: info.ModTime(),
		Size:    info.Size(),
	}, nil
}

// HasChanged 检查文件是否被外部修改
func (fs *FileState) HasChanged() (bool, error) {
	if fs.ModTime.IsZero() {
		// 文件之前不存在，检查现在是否存在
		_, err := os.Stat(fs.Path)
		if err == nil {
			return true, nil // 文件被创建了
		}
		if os.IsNotExist(err) {
			return false, nil // 仍然不存在
		}
		return false, err
	}

	info, err := os.Stat(fs.Path)
	if err != nil {
		if os.IsNotExist(err) {
			return true, nil // 文件被删除了
		}
		return false, err
	}

	// 比较修改时间和大小
	return !info.ModTime().Equal(fs.ModTime) || info.Size() != fs.Size, nil
}

// Update 更新文件状态
func (fs *FileState) Update() error {
	newState, err := GetFileState(fs.Path)
	if err != nil {
		return err
	}
	fs.ModTime = newState.ModTime
	fs.Size = newState.Size
	return nil
}

// AtomicWrite 原子写入文件
// 先写入临时文件，再重命名以保证原子性
func AtomicWrite(path string, data []byte, perm os.FileMode) error {
	// 确保目录存在
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0755); err != nil {
		return fmt.Errorf("failed to create directory: %w", err)
	}

	// 写入临时文件
	tmpPath := path + ".tmp"
	if err := os.WriteFile(tmpPath, data, perm); err != nil {
		return fmt.Errorf("failed to write temp file: %w", err)
	}

	// 原子替换
	if err := os.Rename(tmpPath, path); err != nil {
		os.Remove(tmpPath)
		return fmt.Errorf("failed to rename temp file: %w", err)
	}

	return nil
}

// AtomicWriteString 原子写入字符串到文件
func AtomicWriteString(path string, content string, perm os.FileMode) error {
	return AtomicWrite(path, []byte(content), perm)
}

// SafeWrite 安全写入文件（带备份）
// 如果目标文件存在，先创建备份
func SafeWrite(path string, data []byte, perm os.FileMode) error {
	// 检查目标文件是否存在
	if _, err := os.Stat(path); err == nil {
		// 创建备份
		backupPath := path + ".bak"
		if err := os.Rename(path, backupPath); err != nil {
			return fmt.Errorf("failed to create backup: %w", err)
		}
	}

	// 写入新文件
	if err := os.WriteFile(path, data, perm); err != nil {
		return fmt.Errorf("failed to write file: %w", err)
	}

	return nil
}

// Exists 检查文件是否存在
func Exists(path string) bool {
	_, err := os.Stat(path)
	return err == nil
}

// EnsureDir 确保目录存在
func EnsureDir(path string) error {
	return os.MkdirAll(path, 0755)
}
