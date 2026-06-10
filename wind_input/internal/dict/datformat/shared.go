package datformat

import (
	"sync"

	"github.com/huanfeng/wind_input/internal/dict/hotcache"
)

// 进程级共享 WdatReader 池，语义与 binformat 的共享池一致（见 binformat/shared.go）。
//
// 多个输入方案共用同一份 pinyin.wdat 时，按 FileKey（path+size+mtime）复用
// 同一 mmap 映射，引用计数管理生命周期。WdatReader 的全部解析状态在 Open 时
// 构建完成，查询路径只读，多引擎并发共享安全。
var (
	sharedMu    sync.Mutex
	sharedWdats = map[hotcache.FileKey]*WdatReader{}
)

// OpenWdat 打开 wdat 文件并映射到内存（进程级共享）。
// 同一文件（path+size+mtime 相同）的多次打开返回同一 *WdatReader，
// 由引用计数管理生命周期；每个持有者用完后都应调用 Close。
func OpenWdat(path string) (*WdatReader, error) {
	key := hotcache.MakeFileKey(path)
	sharedMu.Lock()
	defer sharedMu.Unlock()
	if exist, ok := sharedWdats[key]; ok {
		if !exist.closed {
			exist.refs++
			return exist, nil
		}
		delete(sharedWdats, key)
	}
	r, err := openWdat(path)
	if err != nil {
		return nil, err
	}
	r.shareKey = key
	r.refs = 1
	sharedWdats[key] = r
	return r, nil
}
