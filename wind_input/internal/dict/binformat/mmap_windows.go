package binformat

import (
	"fmt"
	"os"
	"unsafe"

	"golang.org/x/sys/windows"
)

// MmapFile 内存映射文件
type MmapFile struct {
	data     []byte
	fileH    windows.Handle
	mappingH windows.Handle
}

// MmapOpen 打开文件并创建内存映射
func MmapOpen(path string) (*MmapFile, error) {
	fi, err := os.Stat(path)
	if err != nil {
		return nil, fmt.Errorf("stat 文件失败: %w", err)
	}
	size := fi.Size()
	if size == 0 {
		return nil, fmt.Errorf("文件为空: %s", path)
	}

	pathUTF16, err := windows.UTF16PtrFromString(path)
	if err != nil {
		return nil, fmt.Errorf("转换路径失败: %w", err)
	}

	// 打开文件
	fileH, err := windows.CreateFile(
		pathUTF16,
		windows.GENERIC_READ,
		windows.FILE_SHARE_READ,
		nil,
		windows.OPEN_EXISTING,
		windows.FILE_ATTRIBUTE_NORMAL,
		0,
	)
	if err != nil {
		return nil, fmt.Errorf("打开文件失败: %w", err)
	}

	// 创建文件映射对象
	mappingH, err := windows.CreateFileMapping(
		fileH,
		nil,
		windows.PAGE_READONLY,
		uint32(size>>32),
		uint32(size),
		nil,
	)
	if err != nil {
		windows.CloseHandle(fileH)
		return nil, fmt.Errorf("创建文件映射失败: %w", err)
	}

	// 映射视图
	addr, err := windows.MapViewOfFile(
		mappingH,
		windows.FILE_MAP_READ,
		0,
		0,
		uintptr(size),
	)
	if err != nil {
		windows.CloseHandle(mappingH)
		windows.CloseHandle(fileH)
		return nil, fmt.Errorf("映射视图失败: %w", err)
	}

	// 将映射的内存区域构造为 []byte 切片
	data := unsafe.Slice((*byte)(unsafe.Pointer(addr)), int(size))

	return &MmapFile{
		data:     data,
		fileH:    fileH,
		mappingH: mappingH,
	}, nil
}

// Data 返回映射的数据
func (m *MmapFile) Data() []byte {
	return m.data
}

// Close 关闭内存映射
func (m *MmapFile) Close() error {
	if m.data != nil {
		if err := windows.UnmapViewOfFile(uintptr(unsafe.Pointer(&m.data[0]))); err != nil {
			return fmt.Errorf("取消映射失败: %w", err)
		}
		m.data = nil
	}
	if m.mappingH != 0 {
		windows.CloseHandle(m.mappingH)
		m.mappingH = 0
	}
	if m.fileH != 0 {
		windows.CloseHandle(m.fileH)
		m.fileH = 0
	}
	return nil
}
