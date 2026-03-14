package binformat

import (
	"fmt"
	"sort"
	"strings"

	"github.com/huanfeng/wind_input/internal/candidate"
)

// DictReader 基于 mmap 的词库读取器
type DictReader struct {
	mmap   *MmapFile
	data   []byte
	header DictFileHeader

	// 预解析偏移
	keyIndexBase  uint32
	entryDataBase uint32
	strPoolBase   uint32

	// 简拼
	hasAbbrev    bool
	abbrevCount  uint32
	abbrevIdxOff uint32
}

// OpenDict 打开二进制词库
func OpenDict(path string) (*DictReader, error) {
	mf, err := MmapOpen(path)
	if err != nil {
		return nil, fmt.Errorf("mmap 打开失败: %w", err)
	}

	data := mf.Data()
	if len(data) < DictFileHeaderSize {
		mf.Close()
		return nil, fmt.Errorf("文件过小: %d bytes", len(data))
	}

	r := &DictReader{
		mmap: mf,
		data: data,
	}

	// 解析文件头
	r.header.Magic = [4]byte{data[0], data[1], data[2], data[3]}
	r.header.Version = byteOrder.Uint32(data[4:8])
	r.header.KeyCount = byteOrder.Uint32(data[8:12])
	r.header.IndexOff = byteOrder.Uint32(data[12:16])
	r.header.DataOff = byteOrder.Uint32(data[16:20])
	r.header.StrOff = byteOrder.Uint32(data[20:24])
	r.header.AbbrevOff = byteOrder.Uint32(data[24:28])

	if err := r.header.Validate(); err != nil {
		mf.Close()
		return nil, err
	}

	// 验证偏移量在文件范围内
	dataLen := uint32(len(data))
	if r.header.IndexOff >= dataLen || r.header.DataOff > dataLen || r.header.StrOff > dataLen {
		mf.Close()
		return nil, fmt.Errorf("文件头包含非法偏移量: IndexOff=%d DataOff=%d StrOff=%d fileLen=%d",
			r.header.IndexOff, r.header.DataOff, r.header.StrOff, dataLen)
	}

	r.keyIndexBase = r.header.IndexOff
	r.entryDataBase = r.header.DataOff
	r.strPoolBase = r.header.StrOff

	// 解析简拼索引头
	if r.header.AbbrevOff > 0 && int(r.header.AbbrevOff)+AbbrevHeaderSize <= len(data) {
		off := r.header.AbbrevOff
		r.abbrevCount = byteOrder.Uint32(data[off : off+4])
		r.abbrevIdxOff = byteOrder.Uint32(data[off+4 : off+8])
		r.hasAbbrev = r.abbrevCount > 0
	}

	return r, nil
}

// Close 关闭读取器
func (r *DictReader) Close() error {
	if r.mmap != nil {
		return r.mmap.Close()
	}
	return nil
}

// KeyCount 返回主索引 key 数量
func (r *DictReader) KeyCount() int {
	return int(r.header.KeyCount)
}

// Lookup 精确查找编码对应的候选词
func (r *DictReader) Lookup(pinyin string) []candidate.Candidate {
	pinyin = strings.ToLower(pinyin)
	idx := r.searchKey(pinyin)
	if idx < 0 {
		return nil
	}
	return r.readEntries(idx)
}

// LookupPhrase 查找短语（将音节拼接后查找）
func (r *DictReader) LookupPhrase(syllables []string) []candidate.Candidate {
	if len(syllables) == 0 {
		return nil
	}
	key := strings.ToLower(strings.Join(syllables, ""))
	return r.Lookup(key)
}

// LookupPrefix 前缀查找
func (r *DictReader) LookupPrefix(prefix string, limit int) []candidate.Candidate {
	prefix = strings.ToLower(prefix)
	if len(prefix) == 0 {
		return nil
	}

	// 二分查找第一个 >= prefix 的 key
	keyCount := int(r.header.KeyCount)
	lo := sort.Search(keyCount, func(i int) bool {
		code := r.readKeyCode(i)
		return code >= prefix
	})

	var results []candidate.Candidate
	for i := lo; i < keyCount; i++ {
		code := r.readKeyCode(i)
		if !strings.HasPrefix(code, prefix) {
			break
		}
		entries := r.readEntries(i)
		results = append(results, entries...)
		if limit > 0 && len(results) >= limit*2 {
			break
		}
	}

	sort.Slice(results, func(i, j int) bool {
		return candidate.Better(results[i], results[j])
	})
	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}
	return results
}

// HasPrefix 检查是否有以 prefix 开头的词条
func (r *DictReader) HasPrefix(prefix string) bool {
	prefix = strings.ToLower(prefix)
	keyCount := int(r.header.KeyCount)
	lo := sort.Search(keyCount, func(i int) bool {
		code := r.readKeyCode(i)
		return code >= prefix
	})
	if lo >= keyCount {
		return false
	}
	code := r.readKeyCode(lo)
	return strings.HasPrefix(code, prefix)
}

// LookupAbbrev 简拼查找
func (r *DictReader) LookupAbbrev(code string, limit int) []candidate.Candidate {
	if !r.hasAbbrev {
		return nil
	}
	code = strings.ToLower(code)
	idx := r.searchAbbrev(code)
	if idx < 0 {
		return nil
	}
	results := r.readAbbrevEntries(idx)
	sort.Slice(results, func(i, j int) bool {
		return candidate.Better(results[i], results[j])
	})
	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}
	return results
}

// EntryCount 返回词条总数（估算值，用于日志）
func (r *DictReader) EntryCount() int {
	// 估算：遍历所有 key 的 entryLen 之和会太慢，用 key 数量代替
	return int(r.header.KeyCount)
}

// LookupPrefixExcludeExact 前缀查找（跳过 code == prefix 的精确匹配）
func (r *DictReader) LookupPrefixExcludeExact(prefix string, limit int) []candidate.Candidate {
	prefix = strings.ToLower(prefix)
	if len(prefix) == 0 {
		return nil
	}

	keyCount := int(r.header.KeyCount)
	lo := sort.Search(keyCount, func(i int) bool {
		code := r.readKeyCode(i)
		return code >= prefix
	})

	var results []candidate.Candidate
	for i := lo; i < keyCount; i++ {
		code := r.readKeyCode(i)
		if !strings.HasPrefix(code, prefix) {
			break
		}
		if code == prefix {
			continue
		}
		entries := r.readEntries(i)
		results = append(results, entries...)
		if limit > 0 && len(results) >= limit*2 {
			break
		}
	}

	sort.Slice(results, func(i, j int) bool {
		return candidate.Better(results[i], results[j])
	})
	if limit > 0 && len(results) > limit {
		results = results[:limit]
	}
	return results
}

// ForEachEntry 顺序遍历所有条目（供 BuildReverseIndex 等使用）
func (r *DictReader) ForEachEntry(fn func(code string, entries []candidate.Candidate)) {
	keyCount := int(r.header.KeyCount)
	for i := 0; i < keyCount; i++ {
		code := r.readKeyCode(i)
		entries := r.readEntries(i)
		fn(code, entries)
	}
}

// ---- 内部方法 ----

// readString 从 StringPool 安全读取字符串
func (r *DictReader) readString(off uint32, length uint16) string {
	start := r.strPoolBase + off
	end := start + uint32(length)
	if end > uint32(len(r.data)) {
		return ""
	}
	return string(r.data[start:end])
}

// searchKey 二分搜索主索引，返回 index 或 -1
func (r *DictReader) searchKey(code string) int {
	keyCount := int(r.header.KeyCount)
	idx := sort.Search(keyCount, func(i int) bool {
		return r.readKeyCode(i) >= code
	})
	if idx < keyCount && r.readKeyCode(idx) == code {
		return idx
	}
	return -1
}

// readKeyCode 读取第 i 个 key 的 code 字符串
func (r *DictReader) readKeyCode(i int) string {
	off := r.keyIndexBase + uint32(i)*DictKeyIndexSize
	codeOff := byteOrder.Uint32(r.data[off : off+4])
	codeLen := byteOrder.Uint16(r.data[off+4 : off+6])
	return r.readString(codeOff, codeLen)
}

// readKeyIndex 读取第 i 个 key 的索引信息
func (r *DictReader) readKeyIndex(i int) (entryOff uint32, entryLen uint16) {
	off := r.keyIndexBase + uint32(i)*DictKeyIndexSize
	entryOff = byteOrder.Uint32(r.data[off+6 : off+10])
	entryLen = byteOrder.Uint16(r.data[off+10 : off+12])
	return
}

// readEntries 读取第 i 个 key 的所有候选词
func (r *DictReader) readEntries(i int) []candidate.Candidate {
	code := r.readKeyCode(i)
	entryOff, entryLen := r.readKeyIndex(i)
	results := make([]candidate.Candidate, 0, entryLen)
	base := r.entryDataBase + entryOff
	for j := uint16(0); j < entryLen; j++ {
		recOff := base + uint32(j)*DictEntryRecordSize
		if recOff+DictEntryRecordSize > uint32(len(r.data)) {
			break
		}
		textOff := byteOrder.Uint32(r.data[recOff : recOff+4])
		textLen := byteOrder.Uint16(r.data[recOff+4 : recOff+6])
		weight := int32(byteOrder.Uint32(r.data[recOff+6 : recOff+10]))

		text := r.readString(textOff, textLen)
		results = append(results, candidate.Candidate{
			Text:         text,
			Code:         code,
			Weight:       int(weight),
			NaturalOrder: int(j), // 二进制文件中的存储顺序即自然顺序
		})
	}
	return results
}

// searchAbbrev 二分搜索简拼索引，返回 index 或 -1
func (r *DictReader) searchAbbrev(code string) int {
	count := int(r.abbrevCount)
	idx := sort.Search(count, func(i int) bool {
		return r.readAbbrevCode(i) >= code
	})
	if idx < count && r.readAbbrevCode(idx) == code {
		return idx
	}
	return -1
}

// readAbbrevCode 读取第 i 个简拼的编码字符串
func (r *DictReader) readAbbrevCode(i int) string {
	off := r.abbrevIdxOff + uint32(i)*AbbrevIndexSize
	abbrevOff := byteOrder.Uint32(r.data[off : off+4])
	abbrevLen := byteOrder.Uint16(r.data[off+4 : off+6])
	return r.readString(abbrevOff, abbrevLen)
}

// readAbbrevEntries 读取第 i 个简拼的所有候选词
func (r *DictReader) readAbbrevEntries(i int) []candidate.Candidate {
	off := r.abbrevIdxOff + uint32(i)*AbbrevIndexSize
	entryOff := byteOrder.Uint32(r.data[off+6 : off+10])
	entryLen := byteOrder.Uint16(r.data[off+10 : off+12])
	base := r.entryDataBase + entryOff
	results := make([]candidate.Candidate, 0, entryLen)
	for j := uint16(0); j < entryLen; j++ {
		recOff := base + uint32(j)*DictEntryRecordSize
		if recOff+DictEntryRecordSize > uint32(len(r.data)) {
			break
		}
		textOff := byteOrder.Uint32(r.data[recOff : recOff+4])
		textLen := byteOrder.Uint16(r.data[recOff+4 : recOff+6])
		weight := int32(byteOrder.Uint32(r.data[recOff+6 : recOff+10]))

		text := r.readString(textOff, textLen)
		results = append(results, candidate.Candidate{
			Text:   text,
			Weight: int(weight),
		})
	}
	return results
}
