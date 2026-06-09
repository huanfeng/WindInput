//go:build !windows

package sysinfo

// availablePhysicalBytes 在非 Windows 平台暂不探测物理内存，返回 0（未知）。
// 上层 LowMemoryMode 在"未知"时回退到默认快速路径，行为与改动前一致。
func availablePhysicalBytes() uint64 {
	return 0
}
