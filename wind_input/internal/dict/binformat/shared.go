package binformat

import (
	"sync"

	"github.com/huanfeng/wind_input/internal/dict/hotcache"
)

// 进程级共享 reader 池。
//
// 背景：多个输入方案会共用同一份词库文件（全拼/双拼/五笔混输都依赖
// pinyin.wdat 与 unigram.wdb）。若每个引擎独立 mmap，同一文件会被映射多次
// （VMMap 实测：切换三个拼音系方案后 pinyin.wdat 被映射 3 次，Commit 与
// Working Set 成倍膨胀）。本池以 FileKey（path+size+mtime）聚合：
//
//   - OpenDict / OpenUnigram 命中池且 reader 未关闭时，引用计数 +1 并返回
//     同一实例，不再新建 mmap；
//   - Close 仅在最后一个持有者关闭时真正释放 mmap，并从池中摘除；
//   - 词库重建走 CloseReadersForPath 强制关闭时，closeFromRegistry 会同步
//     从池中摘除，文件替换后 mtime 变化生成新 FileKey，新旧 reader 自然隔离。
//
// 共享安全性：reader 的全部解析状态在 Open 时构建完成，查询路径只读；
// hot index 本就经由 hotcache 按 FileKey 进程级共享，多引擎并发查询同一
// reader 与原先多 reader 并发查询同一文件等价。
//
// 锁序：sharedMu -> registryMu（openDict 持 sharedMu 期间调 registerReader）。
// CloseReadersForPath 先释放 registryMu 再调 closeFromRegistry（内部取
// sharedMu），两把锁不会同时反向持有。
var (
	sharedMu       sync.Mutex
	sharedDicts    = map[hotcache.FileKey]*DictReader{}
	sharedUnigrams = map[hotcache.FileKey]*UnigramReader{}
)

// OpenDict 打开二进制词库（进程级共享）。
// 同一文件（path+size+mtime 相同）的多次打开返回同一 *DictReader，
// 由引用计数管理生命周期；每个持有者用完后都应调用 Close。
func OpenDict(path string) (*DictReader, error) {
	key := hotcache.MakeFileKey(path)
	sharedMu.Lock()
	defer sharedMu.Unlock()
	if exist, ok := sharedDicts[key]; ok {
		if !exist.isClosed() {
			exist.refs++
			return exist, nil
		}
		delete(sharedDicts, key)
	}
	r, err := openDict(path)
	if err != nil {
		return nil, err
	}
	r.shareKey = key
	r.refs = 1
	sharedDicts[key] = r
	return r, nil
}

// OpenUnigram 打开二进制 unigram 文件（进程级共享，语义同 OpenDict）。
func OpenUnigram(path string) (*UnigramReader, error) {
	key := hotcache.MakeFileKey(path)
	sharedMu.Lock()
	defer sharedMu.Unlock()
	if exist, ok := sharedUnigrams[key]; ok {
		if !exist.isClosed() {
			exist.refs++
			return exist, nil
		}
		delete(sharedUnigrams, key)
	}
	r, err := openUnigram(path)
	if err != nil {
		return nil, err
	}
	r.shareKey = key
	r.refs = 1
	sharedUnigrams[key] = r
	return r, nil
}
