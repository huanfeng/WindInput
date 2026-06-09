// Package sysinfo 提供运行时系统资源探测（当前仅物理内存），
// 用于在低内存环境下让词库生成等高内存操作切换到省内存路径。
package sysinfo

import (
	"os"
	"strconv"
)

// defaultLowMemThresholdMB 是判定"低内存"的默认可用物理内存阈值（MB）。
// 词库生成（拼音 wdat / DAT 构建）优化后的快速路径峰值约 500MB，
// 留约 2x 余量：可用物理内存低于 1GB 时切换到省内存路径（更慢但峰值更低）。
const defaultLowMemThresholdMB = 1024

// AvailablePhysicalMB 返回当前可用物理内存（MB）。
// 无法探测（非 Windows 平台或系统调用失败）时返回 0。
func AvailablePhysicalMB() uint64 {
	b := availablePhysicalBytes()
	if b == 0 {
		return 0
	}
	return b / (1024 * 1024)
}

// LowMemoryMode 报告当前是否应启用省内存路径。
//
// 决策优先级：
//  1. 环境变量 WINDINPUT_FORCE_LOWMEM 显式覆盖：1/true/on 强制开启，
//     0/false/off 强制关闭（用于调试或用户手动指定）。
//  2. 否则按可用物理内存判定：低于阈值（默认 1024MB，可由
//     WINDINPUT_LOWMEM_MB 覆盖）时返回 true。
//  3. 无法探测可用内存（返回 0）时返回 false，保持原有快速路径行为不变。
func LowMemoryMode() bool {
	if v, ok := os.LookupEnv("WINDINPUT_FORCE_LOWMEM"); ok {
		switch v {
		case "1", "true", "TRUE", "on":
			return true
		case "0", "false", "FALSE", "off":
			return false
		}
	}
	avail := AvailablePhysicalMB()
	if avail == 0 {
		return false
	}
	threshold := uint64(defaultLowMemThresholdMB)
	if v := os.Getenv("WINDINPUT_LOWMEM_MB"); v != "" {
		if n, err := strconv.ParseUint(v, 10, 64); err == nil && n > 0 {
			threshold = n
		}
	}
	return avail < threshold
}
