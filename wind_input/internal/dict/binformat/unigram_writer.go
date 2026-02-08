package binformat

import (
	"fmt"
	"io"
	"math"
	"sort"
)

// UnigramEntry 写入时的 unigram 条目
type UnigramEntry struct {
	Word    string
	LogProb float32
}

// UnigramWriter unigram 二进制格式写入器
type UnigramWriter struct {
	entries []UnigramEntry
}

// NewUnigramWriter 创建 unigram 写入器
func NewUnigramWriter() *UnigramWriter {
	return &UnigramWriter{}
}

// Add 添加一个词语及其对数概率
func (w *UnigramWriter) Add(word string, logProb float64) {
	w.entries = append(w.entries, UnigramEntry{
		Word:    word,
		LogProb: float32(logProb),
	})
}

// AddFromFreqs 从频次 map 构建（自动计算 logProb）
func (w *UnigramWriter) AddFromFreqs(freqs map[string]float64) {
	var total float64
	for _, freq := range freqs {
		total += freq
	}
	if total == 0 {
		return
	}
	for word, freq := range freqs {
		w.entries = append(w.entries, UnigramEntry{
			Word:    word,
			LogProb: float32(math.Log(freq / total)),
		})
	}
}

// Write 写入到 writer
func (w *UnigramWriter) Write(out io.Writer) error {
	// 按 key 字典序排序
	sort.Slice(w.entries, func(i, j int) bool {
		return w.entries[i].Word < w.entries[j].Word
	})

	// 构建 StringPool
	pool := newStringPool()
	for i := range w.entries {
		pool.Add(w.entries[i].Word)
	}

	keyCount := uint32(len(w.entries))
	keyIndexSize := keyCount * UnigramKeyIndexSize

	indexOff := uint32(UnigramFileHeaderSize)
	strOff := indexOff + keyIndexSize

	// 写入 Header
	header := UnigramFileHeader{
		Magic:    UnigramMagic,
		Version:  UnigramVersion,
		KeyCount: keyCount,
		IndexOff: indexOff,
		StrOff:   strOff,
	}
	var hBuf [UnigramFileHeaderSize]byte
	copy(hBuf[0:4], header.Magic[:])
	byteOrder.PutUint32(hBuf[4:8], header.Version)
	byteOrder.PutUint32(hBuf[8:12], header.KeyCount)
	byteOrder.PutUint32(hBuf[12:16], header.IndexOff)
	byteOrder.PutUint32(hBuf[16:20], header.StrOff)
	byteOrder.PutUint32(hBuf[20:24], header.Reserved)
	if _, err := out.Write(hBuf[:]); err != nil {
		return fmt.Errorf("写入 unigram 文件头失败: %w", err)
	}

	// 写入 KeyIndex
	for _, e := range w.entries {
		var buf [UnigramKeyIndexSize]byte
		byteOrder.PutUint32(buf[0:4], pool.Offset(e.Word))
		byteOrder.PutUint16(buf[4:6], uint16(len(e.Word)))
		byteOrder.PutUint32(buf[6:10], math.Float32bits(e.LogProb))
		// buf[10:12] reserved = 0
		if _, err := out.Write(buf[:]); err != nil {
			return fmt.Errorf("写入 unigram 索引失败: %w", err)
		}
	}

	// 写入 StringPool
	if _, err := out.Write(pool.Bytes()); err != nil {
		return fmt.Errorf("写入 unigram 字符串池失败: %w", err)
	}

	return nil
}
