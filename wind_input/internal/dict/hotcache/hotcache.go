// Package hotcache 提供进程级共享的"按首字母预聚合的 top-N 候选"缓存。
//
// 设计目的：
//
//	单字母前缀的候选子树极大（拼音 'z' 子树 47k 候选），扫描+排序成本 ~22ms。
//	多个 schema（全拼/双拼/五笔混输）会共用同一份拼音词库文件（pinyin.wdat），
//	若每个 schema 独立维护 hot index，会浪费内存且每次切换方案都要重建。
//	本包以词库文件路径（含 size/mtime 防止过期）作为缓存键，所有指向同一文件的
//	reader 自动共享同一份 hot index。
//
// 用法：
//
//	key := hotcache.MakeFileKey(path)        // 含 path/size/mtime
//	cands := hotcache.GetOrBuild(key, b, n, func() []candidate.Candidate {
//	    return doFullScan(string(b), n)
//	})
//
// 失效策略：缓存键含文件 size+mtime，文件重新生成后 key 变化，老 entry 留在
// map 中（小到可忽略）；若担心累积，可在词库重载入口处主动 ClearByPath。
package hotcache

import (
	"fmt"
	"os"
	"sync"

	"github.com/huanfeng/wind_input/internal/candidate"
)

// FileKey 标识一份词库文件的稳定身份。包含 path + size + mtime，
// 文件重新生成后 key 会变，新旧 reader 自动隔离。
type FileKey string

// MakeFileKey 由文件路径生成 FileKey。无法 stat 时退化为 path 本身。
func MakeFileKey(path string) FileKey {
	info, err := os.Stat(path)
	if err != nil {
		return FileKey(path)
	}
	return FileKey(fmt.Sprintf("%s|%d|%d", path, info.Size(), info.ModTime().UnixNano()))
}

// entry 单个文件对应的 hot index：26 字母槽位，每槽 lazy build。
type entry struct {
	slots [256]slot
}

type slot struct {
	once sync.Once
	list []candidate.Candidate
}

var (
	mu    sync.Mutex
	store = map[FileKey]*entry{}
)

// getEntry 取或创建指定 FileKey 的 entry（带锁，少量竞争）。
func getEntry(key FileKey) *entry {
	mu.Lock()
	defer mu.Unlock()
	e, ok := store[key]
	if !ok {
		e = &entry{}
		store[key] = e
	}
	return e
}

// GetOrBuild 返回 (key, b) 对应的 hot index；首次调用时通过 build 构建。
//
//   - build 必须在内部完成扫描和 top-N 选取，复杂度 O(N log N)；多次调用同一
//     (key, b) 时只执行一次（sync.Once 保证）。
//   - 返回的切片是缓存内的共享只读副本——调用方需视为只读，需要修改时自行拷贝。
func GetOrBuild(key FileKey, b byte, build func() []candidate.Candidate) []candidate.Candidate {
	e := getEntry(key)
	s := &e.slots[b]
	s.once.Do(func() {
		s.list = build()
	})
	return s.list
}

// ClearByPath 由词库重载入口调用，丢弃旧 FileKey 的 entry。多数情况下不需要
// 调用——FileKey 含 mtime，新 reader 会自动落到新的 entry。
func ClearByPath(key FileKey) {
	mu.Lock()
	defer mu.Unlock()
	delete(store, key)
}
