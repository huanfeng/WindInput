package main

import "fmt"

// MemStatsResult 内存统计前端类型
type MemStatsResult struct {
	HeapAlloc    uint64 `json:"heap_alloc"`     // 活跃对象占用字节
	HeapSys      uint64 `json:"heap_sys"`       // 向 OS 申请的堆总量
	HeapIdle     uint64 `json:"heap_idle"`      // 空闲但未归还 OS 的字节
	HeapInuse    uint64 `json:"heap_inuse"`     // 正在使用的 span 字节
	HeapReleased uint64 `json:"heap_released"`  // 已归还 OS 的空闲字节
	HeapObjects  uint64 `json:"heap_objects"`   // 活跃对象数量
	StackInuse   uint64 `json:"stack_inuse"`    // 协程栈占用字节
	StackSys     uint64 `json:"stack_sys"`      // 向 OS 申请的栈字节
	Sys          uint64 `json:"sys"`            // 向 OS 申请的虚拟内存总量
	NumGC        uint32 `json:"num_gc"`         // 累计 GC 次数
	PauseTotalNs uint64 `json:"pause_total_ns"` // GC 累计暂停时间（纳秒）
	GCSys        uint64 `json:"gc_sys"`         // GC 元数据字节
	OtherSys     uint64 `json:"other_sys"`      // 其他系统字节（mspan、mcache 等）
}

// GetMemStats 获取服务端 Go runtime 内存统计（会触发短暂 STW，仅供诊断使用）
func (a *App) GetMemStats() (*MemStatsResult, error) {
	if a.rpcClient == nil {
		return nil, fmt.Errorf("RPC 客户端未初始化")
	}
	reply, err := a.rpcClient.SystemGetMemStats()
	if err != nil {
		return nil, err
	}
	return &MemStatsResult{
		HeapAlloc:    reply.HeapAlloc,
		HeapSys:      reply.HeapSys,
		HeapIdle:     reply.HeapIdle,
		HeapInuse:    reply.HeapInuse,
		HeapReleased: reply.HeapReleased,
		HeapObjects:  reply.HeapObjects,
		StackInuse:   reply.StackInuse,
		StackSys:     reply.StackSys,
		Sys:          reply.Sys,
		NumGC:        reply.NumGC,
		PauseTotalNs: reply.PauseTotalNs,
		GCSys:        reply.GCSys,
		OtherSys:     reply.OtherSys,
	}, nil
}

// DumpHeapProfileResult 导出堆内存 profile 结果
type DumpHeapProfileResult struct {
	Path  string `json:"path"`
	Error string `json:"error,omitempty"`
}

// DumpHeapProfile 触发 GC 并将堆内存 profile 写入服务端 datadir/diag/ 目录
// 返回值中 Error 非空表示失败；Path 为实际写入路径，供用户用 go tool pprof 分析
func (a *App) DumpHeapProfile() DumpHeapProfileResult {
	if a.rpcClient == nil {
		return DumpHeapProfileResult{Error: "RPC 客户端未初始化"}
	}
	reply, err := a.rpcClient.SystemDumpHeapProfile("")
	if err != nil {
		return DumpHeapProfileResult{Error: err.Error()}
	}
	return DumpHeapProfileResult{Path: reply.Path}
}
