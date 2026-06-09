//go:build windows

package sysinfo

import (
	"unsafe"

	"golang.org/x/sys/windows"
)

// memoryStatusEx 对应 Win32 MEMORYSTATUSEX 结构（kernel32!GlobalMemoryStatusEx）。
// 字段顺序与布局须与 MSDN 完全一致，否则读到的内存值无意义。
// amd64 下两个 uint32（Length/MemoryLoad）恰好凑足 8 字节，后续 uint64
// 字段自然 8 字节对齐，无隐式填充，结构体共 64 字节。
type memoryStatusEx struct {
	Length               uint32
	MemoryLoad           uint32
	TotalPhys            uint64
	AvailPhys            uint64
	TotalPageFile        uint64
	AvailPageFile        uint64
	TotalVirtual         uint64
	AvailVirtual         uint64
	AvailExtendedVirtual uint64
}

var (
	modkernel32              = windows.NewLazySystemDLL("kernel32.dll")
	procGlobalMemoryStatusEx = modkernel32.NewProc("GlobalMemoryStatusEx")
)

// availablePhysicalBytes 通过 GlobalMemoryStatusEx 读取可用物理内存字节数。
// 调用失败时返回 0（视作"未知"，上层据此回退到默认快速路径）。
func availablePhysicalBytes() uint64 {
	var m memoryStatusEx
	m.Length = uint32(unsafe.Sizeof(m))
	r, _, _ := procGlobalMemoryStatusEx.Call(uintptr(unsafe.Pointer(&m)))
	if r == 0 {
		return 0
	}
	return m.AvailPhys
}
