package datformat

import (
	"encoding/binary"
	"fmt"
	"sort"
	"unsafe"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict/binformat"
	"github.com/huanfeng/wind_input/internal/dict/hotcache"
)

// HotPrefixIndexN 单字母 prefix hot index 的容量。详见 binformat.HotPrefixIndexN。
const HotPrefixIndexN = 500

// WdatReader 通过 mmap 打开 wdat 文件，零反序列化读取 DAT 数组
type WdatReader struct {
	mmap   *binformat.MmapFile
	data   []byte
	header WdatFileHeader

	datBase  []int32 // mmap 零拷贝映射
	datCheck []int32

	leafBase  uint32 // LeafTable 在文件中的偏移
	entryBase uint32 // EntryRecords 在文件中的偏移
	strBase   uint32 // StringPool 在文件中的偏移

	// 字符映射
	charMap [256]int32
	maxCode int32

	// 简拼
	hasAbbrev       bool
	abbrevBase      []int32
	abbrevCheck     []int32
	abbrevLeafBase  uint32
	abbrevEntryBase uint32
	abbrevCharMap   [256]int32
	abbrevMaxCode   int32

	// 进程级 hot index 缓存键（按 path+size+mtime 聚合，多 reader 共享同一份）
	hotKey hotcache.FileKey
}

// OpenWdat 打开 wdat 文件并映射到内存
func OpenWdat(path string) (*WdatReader, error) {
	mf, err := binformat.MmapOpen(path)
	if err != nil {
		return nil, fmt.Errorf("mmap open: %w", err)
	}

	data := mf.Data()
	if len(data) < WdatFileHeaderSize {
		mf.Close()
		return nil, fmt.Errorf("文件太小: %d bytes", len(data))
	}

	r := &WdatReader{
		mmap:   mf,
		data:   data,
		hotKey: hotcache.MakeFileKey(path),
	}

	// 解析文件头
	r.header.Magic = [4]byte(data[0:4])
	r.header.Version = byteOrder.Uint32(data[4:8])
	r.header.DATSize = byteOrder.Uint32(data[8:12])
	r.header.LeafCount = byteOrder.Uint32(data[12:16])
	r.header.DATOff = byteOrder.Uint32(data[16:20])
	r.header.LeafOff = byteOrder.Uint32(data[20:24])
	r.header.EntryOff = byteOrder.Uint32(data[24:28])
	r.header.StrOff = byteOrder.Uint32(data[28:32])
	r.header.AbbrevOff = byteOrder.Uint32(data[32:36])
	r.header.MetaOff = byteOrder.Uint32(data[36:40])
	r.header.EntryCount = byteOrder.Uint32(data[40:44])
	r.header.CharMapOff = byteOrder.Uint32(data[44:48])

	if err := r.header.Validate(); err != nil {
		mf.Close()
		return nil, fmt.Errorf("文件头校验失败: %w", err)
	}

	// 零拷贝映射 DAT 数组
	datOff := int(r.header.DATOff)
	datSize := int(r.header.DATSize)
	r.datBase = unsafe.Slice((*int32)(unsafe.Pointer(&data[datOff])), datSize)
	r.datCheck = unsafe.Slice((*int32)(unsafe.Pointer(&data[datOff+datSize*4])), datSize)

	r.leafBase = r.header.LeafOff
	r.entryBase = r.header.EntryOff
	r.strBase = r.header.StrOff

	// 简拼区
	if r.header.AbbrevOff > 0 {
		abbOff := int(r.header.AbbrevOff)
		if abbOff+AbbrevSectionSize > len(data) {
			mf.Close()
			return nil, fmt.Errorf("简拼区段越界")
		}
		var abbSec AbbrevSection
		abbSec.DATSize = byteOrder.Uint32(data[abbOff : abbOff+4])
		abbSec.LeafCount = byteOrder.Uint32(data[abbOff+4 : abbOff+8])
		abbSec.DATOff = byteOrder.Uint32(data[abbOff+8 : abbOff+12])
		abbSec.LeafOff = byteOrder.Uint32(data[abbOff+12 : abbOff+16])

		r.hasAbbrev = true
		abbDATOff := int(abbSec.DATOff)
		abbDATSize := int(abbSec.DATSize)
		r.abbrevBase = unsafe.Slice((*int32)(unsafe.Pointer(&data[abbDATOff])), abbDATSize)
		r.abbrevCheck = unsafe.Slice((*int32)(unsafe.Pointer(&data[abbDATOff+abbDATSize*4])), abbDATSize)
		r.abbrevLeafBase = abbSec.LeafOff
		r.abbrevEntryBase = abbSec.LeafOff + uint32(abbSec.LeafCount)*LeafRecordSize
	} else {
		r.abbrevCharMap = IdentityCharMap()
		r.abbrevMaxCode = 255
	}

	// 读取简拼 CharMap（位于主 CharMap 之前）
	if r.hasAbbrev && r.header.Version >= WdatVersion && r.header.CharMapOff > 0 {
		abbCmOff := int(r.header.CharMapOff) - CharMapSectionSize
		if abbCmOff >= 0 && abbCmOff+CharMapSectionSize <= len(data) {
			r.abbrevMaxCode = int32(byteOrder.Uint32(data[abbCmOff : abbCmOff+4]))
			for i := 0; i < 256; i++ {
				off := abbCmOff + 4 + i*4
				r.abbrevCharMap[i] = int32(byteOrder.Uint32(data[off : off+4]))
			}
		}
	}

	// 读取主 CharMap
	if r.header.Version >= WdatVersion && r.header.CharMapOff > 0 {
		cmOff := int(r.header.CharMapOff)
		if cmOff+CharMapSectionSize > len(data) {
			mf.Close()
			return nil, fmt.Errorf("CharMap 区段越界")
		}
		r.maxCode = int32(byteOrder.Uint32(data[cmOff : cmOff+4]))
		for i := 0; i < 256; i++ {
			off := cmOff + 4 + i*4
			r.charMap[i] = int32(byteOrder.Uint32(data[off : off+4]))
		}
	} else {
		// v1 兼容：使用恒等映射
		r.charMap = IdentityCharMap()
		r.maxCode = 255
	}

	return r, nil
}

// Close 关闭文件映射
func (r *WdatReader) Close() error {
	if r.mmap != nil {
		return r.mmap.Close()
	}
	return nil
}

// KeyCount 返回主 DAT 中的 key 数量
func (r *WdatReader) KeyCount() int {
	return int(r.header.LeafCount)
}

// mainDAT 构建临时主 DAT 引用
func (r *WdatReader) mainDAT() *DAT {
	return &DAT{Base: r.datBase, Check: r.datCheck, Size: int(r.header.DATSize), CharMap: r.charMap, MaxCode: r.maxCode}
}

// abbrevDAT 构建临时简拼 DAT 引用
func (r *WdatReader) abbrevDAT() *DAT {
	return &DAT{Base: r.abbrevBase, Check: r.abbrevCheck, Size: len(r.abbrevBase), CharMap: r.abbrevCharMap, MaxCode: r.abbrevMaxCode}
}

// readLeaf 从指定区域读取 LeafRecord
func (r *WdatReader) readLeaf(leafBase uint32, leafIdx uint32) LeafRecord {
	off := int(leafBase) + int(leafIdx)*LeafRecordSize
	return LeafRecord{
		EntryOff: byteOrder.Uint32(r.data[off : off+4]),
		EntryLen: byteOrder.Uint16(r.data[off+4 : off+6]),
	}
}

// appendEntries 将 LeafRecord 对应的候选词追加到 dst 后返回新切片。
// 调用方提供 dst 可避免每次新建底层数组——前缀扫描/简拼合并等聚合场景大量受益。
func (r *WdatReader) appendEntries(dst []candidate.Candidate, entryBase uint32, leaf LeafRecord, code string) []candidate.Candidate {
	count := int(leaf.EntryLen)
	if cap(dst)-len(dst) < count {
		grown := make([]candidate.Candidate, len(dst), len(dst)+count)
		copy(grown, dst)
		dst = grown
	}
	base := int(entryBase) + int(leaf.EntryOff)
	for i := range count {
		eOff := base + i*EntryRecordSize
		textOff := byteOrder.Uint32(r.data[eOff : eOff+4])
		textLen := byteOrder.Uint16(r.data[eOff+4 : eOff+6])
		weight := int32(binary.LittleEndian.Uint32(r.data[eOff+6 : eOff+10]))

		strStart := int(r.strBase) + int(textOff)
		text := string(r.data[strStart : strStart+int(textLen)])

		dst = append(dst, candidate.Candidate{
			Text:         text,
			Code:         code,
			Weight:       int(weight),
			NaturalOrder: i,
		})
	}
	return dst
}

// readEntries 是 appendEntries 的便捷封装：分配新切片并返回。
// 用于不需要累积的调用点（如单次 Lookup）。
func (r *WdatReader) readEntries(entryBase uint32, leaf LeafRecord, code string) []candidate.Candidate {
	return r.appendEntries(nil, entryBase, leaf, code)
}

// Lookup 精确查找编码，返回候选词列表
func (r *WdatReader) Lookup(code string) []candidate.Candidate {
	dat := r.mainDAT()
	leafIdx, found := dat.ExactMatch(code)
	if !found {
		return nil
	}
	leaf := r.readLeaf(r.leafBase, leafIdx)
	return r.readEntries(r.entryBase, leaf, code)
}

// LookupPrefix 前缀查找，收集所有匹配前缀的候选词，按权重排序后截断到 limit
//
// 单字母 prefix 走 hot index 快速路径——每首字母对应的 top-N 预聚合结果存于
// 进程级 hotcache，多个指向同一 wdat 文件的 reader 共享。
//
// 多字母 prefix 走 scanPrefix：跨叶节点权重无序，必须遍历整棵子树。
// limit > 0 用 min-heap top-K，limit == 0 完整排序保留"无限制"语义。
func (r *WdatReader) LookupPrefix(prefix string, limit int) []candidate.Candidate {
	if len(prefix) == 1 && limit > 0 && limit <= HotPrefixIndexN {
		return r.hotPrefixSlice(prefix[0], limit)
	}
	return r.scanPrefix(prefix, limit)
}

// scanPrefix 扫描整个 prefix 子树并按 limit 选取 top-K（或完整排序）。
func (r *WdatReader) scanPrefix(prefix string, limit int) []candidate.Candidate {
	dat := r.mainDAT()
	leafIndices := dat.PrefixCollect(prefix, 0)
	if len(leafIndices) == 0 {
		return nil
	}

	if limit > 0 {
		picker := newTopKPicker(limit)
		for _, leafIdx := range leafIndices {
			leaf := r.readLeaf(r.leafBase, leafIdx)
			for _, e := range r.readEntries(r.entryBase, leaf, "") {
				picker.offer(e)
			}
		}
		return picker.sorted()
	}

	var all []candidate.Candidate
	for _, leafIdx := range leafIndices {
		leaf := r.readLeaf(r.leafBase, leafIdx)
		all = r.appendEntries(all, r.entryBase, leaf, "")
	}
	sort.Slice(all, func(i, j int) bool {
		return candidate.Better(all[i], all[j])
	})
	return all
}

// hotPrefixSlice 从 hotcache 取 hot index 并截取前 limit 条。返回深拷贝。
func (r *WdatReader) hotPrefixSlice(b byte, limit int) []candidate.Candidate {
	cached := hotcache.GetOrBuild(r.hotKey, b, func() []candidate.Candidate {
		return r.scanPrefix(string([]byte{b}), HotPrefixIndexN)
	})
	if limit > len(cached) {
		limit = len(cached)
	}
	out := make([]candidate.Candidate, limit)
	copy(out, cached[:limit])
	return out
}

// LookupAbbrev 简拼查找（精确长度匹配）。
//
// 简拼 code 的声母数即目标词字数（如 "nh"=2 声母→两字词），因此只做 ExactMatch，
// 不展开 code* 前缀子树。旧实现用 PrefixCollect(code, 0) 收集整棵子树的全部叶子，
// 对短简拼（如 "sf"）会扫描数千叶子 / 上万词条再排序截断，造成 50~100ms 卡顿；
// 且把 "sf" 召回 "sfg"(三字) 等更长简拼词，既是噪声也不符合主流"N 声母 = N 字"语义。
// 与 binformat DictReader.LookupAbbrev 的精确匹配行为对齐。
func (r *WdatReader) LookupAbbrev(code string, limit int) []candidate.Candidate {
	if !r.hasAbbrev {
		return nil
	}
	dat := r.abbrevDAT()
	leafIdx, found := dat.ExactMatch(code)
	if !found {
		return nil
	}

	leaf := r.readLeaf(r.abbrevLeafBase, leafIdx)
	all := r.appendEntries(nil, r.abbrevEntryBase, leaf, code)

	sort.Slice(all, func(i, j int) bool {
		return candidate.Better(all[i], all[j])
	})

	if limit > 0 && len(all) > limit {
		all = all[:limit]
	}
	return all
}

// HasPrefix 检查主 DAT 中是否存在指定前缀
func (r *WdatReader) HasPrefix(prefix string) bool {
	dat := r.mainDAT()
	_, found := dat.walkPrefix(prefix)
	return found
}

// WdatCursor 前缀遍历游标
type WdatCursor struct {
	reader *WdatReader
	inner  *DATCursor
}

// PrefixCursor 创建前缀遍历游标
func (r *WdatReader) PrefixCursor(prefix string) *WdatCursor {
	dat := r.mainDAT()
	inner := dat.PrefixCursor(prefix)
	return &WdatCursor{reader: r, inner: inner}
}

// NextEntries 取下一批候选词
func (c *WdatCursor) NextEntries(maxLeaves int) []candidate.Candidate {
	leafIndices := c.inner.Next(maxLeaves)
	if len(leafIndices) == 0 {
		return nil
	}

	var all []candidate.Candidate
	for _, leafIdx := range leafIndices {
		leaf := c.reader.readLeaf(c.reader.leafBase, leafIdx)
		entries := c.reader.readEntries(c.reader.entryBase, leaf, "")
		all = append(all, entries...)
	}
	return all
}

// HasMore 是否还有更多
func (c *WdatCursor) HasMore() bool {
	return c.inner.HasMore()
}

// Close 释放资源
func (c *WdatCursor) Close() {
	c.inner.Close()
}
