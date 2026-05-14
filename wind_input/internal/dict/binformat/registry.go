package binformat

import (
	"path/filepath"
	"sync"
)

// 进程级 mmap reader 注册表。
//
// 背景：Windows 上一旦文件被 mmap，对它的 rename/remove 都会被拒（"Access is denied"），
// 这导致 dictcache 的"原子替换 wdb"在重建词库时撞上自己进程内 reader 的 mmap 锁——
// 表现为方案切换循环失败、用户无法输入。
//
// 解决方式：所有 binformat reader（DictReader/UnigramReader）在打开/关闭时
// 注册/注销到这张全局表；dictcache.atomicWriteWdb 在 rename 之前调用
// CloseReadersForPath 强制释放本进程持有的同路径 mmap，使替换得以成功。
//
// 副作用：被强制关闭的 reader 所在 engine 后续查询会返回空结果（reader 的查询
// 路径会检测到 mmap 已释放，安全返回 nil 而非崩溃）。这是已知代价，下一次方案
// 切换重新加载 reader 后即恢复。
//
// 路径键统一通过 filepath.Clean 规范化，避免 "a/b.wdb" 与 "a\\b.wdb" 视为不同键。

type closeable interface {
	closeFromRegistry() error
}

var (
	registryMu sync.Mutex
	registry   = map[string][]closeable{}
)

func registryKey(path string) string {
	return filepath.Clean(path)
}

// registerReader 由 reader 在打开成功后调用，登记一份引用。
func registerReader(path string, r closeable) {
	key := registryKey(path)
	registryMu.Lock()
	registry[key] = append(registry[key], r)
	registryMu.Unlock()
}

// unregisterReader 由 reader 在 Close 时调用，移除自身（按身份比对）。
// 多次调用安全（找不到即返回）。
func unregisterReader(path string, r closeable) {
	key := registryKey(path)
	registryMu.Lock()
	defer registryMu.Unlock()
	list := registry[key]
	for i, item := range list {
		if item == r {
			registry[key] = append(list[:i], list[i+1:]...)
			if len(registry[key]) == 0 {
				delete(registry, key)
			}
			return
		}
	}
}

// CloseReadersForPath 强制关闭本进程内所有指向该路径的 mmap reader，
// 返回被关闭的 reader 数量。dictcache 在原子替换 wdb 前调用本函数，
// 释放 Windows 上对目标文件的 mmap 锁。
//
// 各 reader 的 Close 实现需保证幂等：注册表关闭后，reader 持有者再次 Close
// 不会重复释放底层 mmap。
func CloseReadersForPath(path string) int {
	key := registryKey(path)
	registryMu.Lock()
	list := registry[key]
	delete(registry, key)
	registryMu.Unlock()

	for _, r := range list {
		_ = r.closeFromRegistry()
	}
	return len(list)
}
