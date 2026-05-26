//go:build !windows

package binformat

import (
	"fmt"
	"os"
	"syscall"
)

// mmap_unix.go 提供 darwin/linux 上的 mmap 实现, 与 mmap_windows.go 接口一致。
//
// 用 syscall.Mmap (POSIX mmap PROT_READ + MAP_SHARED) 而非 golang.org/x/sys/unix,
// 避免新增依赖; syscall 包在 darwin/linux 上均提供 Mmap/Munmap, 行为充分稳定。

// MmapFile 内存映射文件 (POSIX 平台)。
type MmapFile struct {
	data []byte
	file *os.File
}

// MmapOpen 打开文件并创建只读 mmap。
func MmapOpen(path string) (*MmapFile, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, fmt.Errorf("open file: %w", err)
	}
	fi, err := f.Stat()
	if err != nil {
		_ = f.Close()
		return nil, fmt.Errorf("stat file: %w", err)
	}
	size := fi.Size()
	if size == 0 {
		_ = f.Close()
		return nil, fmt.Errorf("文件为空: %s", path)
	}

	data, err := syscall.Mmap(int(f.Fd()), 0, int(size),
		syscall.PROT_READ, syscall.MAP_SHARED)
	if err != nil {
		_ = f.Close()
		return nil, fmt.Errorf("mmap: %w", err)
	}
	return &MmapFile{data: data, file: f}, nil
}

// Data 返回映射区域只读切片。
func (m *MmapFile) Data() []byte { return m.data }

// Close 解除映射并关闭文件句柄。
func (m *MmapFile) Close() error {
	var firstErr error
	if len(m.data) > 0 {
		if err := syscall.Munmap(m.data); err != nil {
			firstErr = fmt.Errorf("munmap: %w", err)
		}
		m.data = nil
	}
	if m.file != nil {
		if err := m.file.Close(); err != nil && firstErr == nil {
			firstErr = fmt.Errorf("close file: %w", err)
		}
		m.file = nil
	}
	return firstErr
}
