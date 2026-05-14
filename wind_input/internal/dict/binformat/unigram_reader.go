package binformat

import (
	"fmt"
	"math"
	"sort"
	"sync"
)

// UnigramReader 基于 mmap 的 unigram 读取器
type UnigramReader struct {
	mmap   *MmapFile
	data   []byte
	header UnigramFileHeader

	keyIndexBase uint32
	strPoolBase  uint32

	// 缓存的统计信息
	minProb float64

	// path 用于在 Close 时反注册；closeOnce 保证幂等
	path      string
	closeOnce sync.Once
}

// OpenUnigram 打开二进制 unigram 文件
func OpenUnigram(path string) (*UnigramReader, error) {
	mf, err := MmapOpen(path)
	if err != nil {
		return nil, fmt.Errorf("mmap 打开失败: %w", err)
	}

	data := mf.Data()
	if len(data) < UnigramFileHeaderSize {
		mf.Close()
		return nil, fmt.Errorf("文件过小: %d bytes", len(data))
	}

	r := &UnigramReader{
		mmap: mf,
		data: data,
		path: path,
	}

	// 解析文件头
	r.header.Magic = [4]byte{data[0], data[1], data[2], data[3]}
	r.header.Version = byteOrder.Uint32(data[4:8])
	r.header.KeyCount = byteOrder.Uint32(data[8:12])
	r.header.IndexOff = byteOrder.Uint32(data[12:16])
	r.header.StrOff = byteOrder.Uint32(data[16:20])
	r.header.MinFreqMark = byteOrder.Uint32(data[20:24])

	if err := r.header.Validate(); err != nil {
		mf.Close()
		return nil, err
	}

	// 验证偏移量
	dataLen := uint32(len(data))
	if r.header.IndexOff >= dataLen || r.header.StrOff > dataLen {
		mf.Close()
		return nil, fmt.Errorf("文件头包含非法偏移量: IndexOff=%d StrOff=%d fileLen=%d",
			r.header.IndexOff, r.header.StrOff, dataLen)
	}

	r.keyIndexBase = r.header.IndexOff
	r.strPoolBase = r.header.StrOff

	// 未登录词使用固定最小概率
	r.minProb = -20.0

	registerReader(path, r)
	return r, nil
}

// Close 关闭读取器（幂等）。
// 同时从进程级注册表移除自身；data/mmap 置 nil，后续查询安全返回 minProb。
func (r *UnigramReader) Close() error {
	var err error
	r.closeOnce.Do(func() {
		err = r.releaseLocked()
		unregisterReader(r.path, r)
	})
	return err
}

// closeFromRegistry 由注册表强制关闭时调用：释放 mmap 但跳过注销。
func (r *UnigramReader) closeFromRegistry() error {
	var err error
	r.closeOnce.Do(func() {
		err = r.releaseLocked()
	})
	return err
}

// releaseLocked 释放 mmap 并把内部指针置 nil。
func (r *UnigramReader) releaseLocked() error {
	mf := r.mmap
	r.mmap = nil
	r.data = nil
	if mf != nil {
		return mf.Close()
	}
	return nil
}

// isClosed 报告 reader 是否已被释放。
func (r *UnigramReader) isClosed() bool {
	return r.data == nil
}

// Size 返回词汇量
func (r *UnigramReader) Size() int {
	return int(r.header.KeyCount)
}

// MinFreqMark 返回 wdb 生成时的 min-freq 标记，运行时用于判断是否需要重建
func (r *UnigramReader) MinFreqMark() uint32 {
	return r.header.MinFreqMark
}

// LogProb 获取词语的对数概率
func (r *UnigramReader) LogProb(word string) float64 {
	idx := r.searchKey(word)
	if idx < 0 {
		return r.minProb
	}
	return r.readLogProb(idx)
}

// Contains 检查词语是否在模型中
func (r *UnigramReader) Contains(word string) bool {
	return r.searchKey(word) >= 0
}

// CharBasedScore 基于单字频率估算词组常见度
func (r *UnigramReader) CharBasedScore(word string) float64 {
	runes := []rune(word)
	if len(runes) == 0 {
		return r.minProb
	}
	var sum float64
	for _, ru := range runes {
		sum += r.LogProb(string(ru))
	}
	return sum / float64(len(runes))
}

// MinProb 返回最小概率值
func (r *UnigramReader) MinProb() float64 {
	return r.minProb
}

// ---- 内部方法 ----

// searchKey 二分搜索，返回 index 或 -1
func (r *UnigramReader) searchKey(word string) int {
	if r.isClosed() {
		return -1
	}
	keyCount := int(r.header.KeyCount)
	idx := sort.Search(keyCount, func(i int) bool {
		return r.readKey(i) >= word
	})
	if idx < keyCount && r.readKey(idx) == word {
		return idx
	}
	return -1
}

// readKey 读取第 i 个 key
func (r *UnigramReader) readKey(i int) string {
	if r.isClosed() {
		return ""
	}
	off := r.keyIndexBase + uint32(i)*UnigramKeyIndexSize
	keyOff := byteOrder.Uint32(r.data[off : off+4])
	keyLen := byteOrder.Uint16(r.data[off+4 : off+6])
	strStart := r.strPoolBase + keyOff
	strEnd := strStart + uint32(keyLen)
	if strEnd > uint32(len(r.data)) {
		return ""
	}
	return string(r.data[strStart:strEnd])
}

// readLogProb 读取第 i 个 key 的 logProb
func (r *UnigramReader) readLogProb(i int) float64 {
	if r.isClosed() {
		return r.minProb
	}
	off := r.keyIndexBase + uint32(i)*UnigramKeyIndexSize + 6
	bits := byteOrder.Uint32(r.data[off : off+4])
	return float64(math.Float32frombits(bits))
}
