package binformat

import (
	"fmt"
	"sort"
	"strings"
	"sync"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict/hotcache"
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

	// 版本相关
	entryRecordSize uint32 // V2: 10, V3: 14

	// 简拼
	hasAbbrev    bool
	abbrevCount  uint32
	abbrevIdxOff uint32

	// 进程级 hot index 缓存键（按 path+size+mtime 聚合，多 reader 共享同一份）
	hotKey hotcache.FileKey

	// path 用于在 Close 时反注册；closeOnce 保证幂等
	path      string
	closeOnce sync.Once
}

// HotPrefixIndexN 是单字母前缀 hot index 缓存的容量。
// 取 500 比生产 prefixSafeLimit (200) 留余量，覆盖少量翻页扩展场景。
const HotPrefixIndexN = 500

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
		mmap:   mf,
		data:   data,
		hotKey: hotcache.MakeFileKey(path),
		path:   path,
	}

	// 解析文件头
	r.header.Magic = [4]byte{data[0], data[1], data[2], data[3]}
	r.header.Version = byteOrder.Uint32(data[4:8])
	r.header.KeyCount = byteOrder.Uint32(data[8:12])
	r.header.IndexOff = byteOrder.Uint32(data[12:16])
	r.header.DataOff = byteOrder.Uint32(data[16:20])
	r.header.StrOff = byteOrder.Uint32(data[20:24])
	r.header.AbbrevOff = byteOrder.Uint32(data[24:28])
	r.header.MetaOff = byteOrder.Uint32(data[28:32])

	if err := r.header.Validate(); err != nil {
		mf.Close()
		return nil, err
	}

	// 验证偏移量在文件范围内（检测截断的缓存文件）
	dataLen := uint32(len(data))
	if r.header.IndexOff >= dataLen || r.header.DataOff > dataLen || r.header.StrOff > dataLen {
		mf.Close()
		return nil, fmt.Errorf("文件头包含非法偏移量: IndexOff=%d DataOff=%d StrOff=%d fileLen=%d",
			r.header.IndexOff, r.header.DataOff, r.header.StrOff, dataLen)
	}
	if r.header.AbbrevOff > 0 && r.header.AbbrevOff > dataLen {
		mf.Close()
		return nil, fmt.Errorf("文件可能被截断: AbbrevOff=%d fileLen=%d", r.header.AbbrevOff, dataLen)
	}
	if r.header.MetaOff > 0 && r.header.MetaOff > dataLen {
		mf.Close()
		return nil, fmt.Errorf("文件可能被截断: MetaOff=%d fileLen=%d", r.header.MetaOff, dataLen)
	}

	r.keyIndexBase = r.header.IndexOff
	r.entryDataBase = r.header.DataOff
	r.strPoolBase = r.header.StrOff

	// V3 新增 Order 字段，记录大小从 10 变为 14
	if r.header.Version >= 3 {
		r.entryRecordSize = DictEntryRecordSize
	} else {
		r.entryRecordSize = DictEntryRecordSizeV2
	}

	// 解析简拼索引头
	if r.header.AbbrevOff > 0 && int(r.header.AbbrevOff)+AbbrevHeaderSize <= len(data) {
		off := r.header.AbbrevOff
		r.abbrevCount = byteOrder.Uint32(data[off : off+4])
		r.abbrevIdxOff = byteOrder.Uint32(data[off+4 : off+8])
		r.hasAbbrev = r.abbrevCount > 0
	}

	registerReader(path, r)
	return r, nil
}

// Close 关闭读取器（幂等）。
// 同时从进程级注册表移除自身；data/mmap 字段被置 nil，后续查询路径全部短路返回空。
func (r *DictReader) Close() error {
	var err error
	r.closeOnce.Do(func() {
		err = r.releaseLocked()
		unregisterReader(r.path, r)
	})
	return err
}

// closeFromRegistry 由注册表强制关闭时调用：释放 mmap 但跳过注销
// （注册表本身已经把条目摘出 map 了，重复 unregister 是 no-op，但避免再走一遍）。
func (r *DictReader) closeFromRegistry() error {
	var err error
	r.closeOnce.Do(func() {
		err = r.releaseLocked()
	})
	return err
}

// releaseLocked 真正释放 mmap 并把内部指针置 nil，使后续查询安全返回空。
// 仅在 closeOnce 内部调用，外部应通过 Close/closeFromRegistry 间接触发。
func (r *DictReader) releaseLocked() error {
	mf := r.mmap
	r.mmap = nil
	r.data = nil
	if mf != nil {
		return mf.Close()
	}
	return nil
}

// isClosed 报告 reader 是否已被释放。查询路径据此短路，避免访问已释放的 mmap 内存。
func (r *DictReader) isClosed() bool {
	return r.data == nil
}

// ReadMeta 读取嵌入的元数据（JSON 格式）
// 返回 nil 表示无元数据
func (r *DictReader) ReadMeta() []byte {
	if r.header.MetaOff == 0 {
		return nil
	}
	off := r.header.MetaOff
	if int(off)+MetaHeaderSize > len(r.data) {
		return nil
	}
	dataLen := byteOrder.Uint32(r.data[off : off+4])
	start := off + MetaHeaderSize
	end := start + dataLen
	if int(end) > len(r.data) {
		return nil
	}
	return r.data[start:end]
}

// HasMeta 是否包含元数据
func (r *DictReader) HasMeta() bool {
	return r.header.MetaOff > 0
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
//
// 单字母前缀走 hot index 快速路径——每首字母对应的 top-N 候选预聚合到进程级
// hotcache 中，多个 reader 指向同一文件时共享。
//
// 多字母前缀走 scanPrefix：跨 key 候选权重无序，提前 break 会盲选字典序前若干
// key、丢失高权重候选；因此扫描整个子树。limit > 0 时用 min-heap top-K，
// limit == 0 时完整排序保持"无限制"语义。
func (r *DictReader) LookupPrefix(prefix string, limit int) []candidate.Candidate {
	prefix = strings.ToLower(prefix)
	if len(prefix) == 0 {
		return nil
	}
	if len(prefix) == 1 && limit > 0 && limit <= HotPrefixIndexN {
		return r.hotPrefixSlice(prefix[0], limit)
	}
	return r.scanPrefix(prefix, limit, false)
}

// scanPrefix 扫描整个 prefix 子树并返回候选；excludeExact=true 时跳过 code==prefix。
func (r *DictReader) scanPrefix(prefix string, limit int, excludeExact bool) []candidate.Candidate {
	keyCount := int(r.header.KeyCount)
	lo := sort.Search(keyCount, func(i int) bool {
		code := r.readKeyCode(i)
		return code >= prefix
	})

	if limit > 0 {
		picker := newTopKPicker(limit)
		for i := lo; i < keyCount; i++ {
			code := r.readKeyCode(i)
			if !strings.HasPrefix(code, prefix) {
				break
			}
			if excludeExact && code == prefix {
				continue
			}
			for _, e := range r.readEntries(i) {
				picker.offer(e)
			}
		}
		return picker.sorted()
	}

	var results []candidate.Candidate
	for i := lo; i < keyCount; i++ {
		code := r.readKeyCode(i)
		if !strings.HasPrefix(code, prefix) {
			break
		}
		if excludeExact && code == prefix {
			continue
		}
		results = append(results, r.readEntries(i)...)
	}
	sort.SliceStable(results, func(i, j int) bool {
		return candidate.Better(results[i], results[j])
	})
	return results
}

// hotPrefixSlice 从 hotcache 取 hot index 并截取前 limit 条返回。
// 缓存内的切片视为只读——拷贝一份返回，避免上层修改污染缓存。
func (r *DictReader) hotPrefixSlice(b byte, limit int) []candidate.Candidate {
	cached := hotcache.GetOrBuild(r.hotKey, b, func() []candidate.Candidate {
		return r.scanPrefix(string([]byte{b}), HotPrefixIndexN, false)
	})
	if limit > len(cached) {
		limit = len(cached)
	}
	out := make([]candidate.Candidate, limit)
	copy(out, cached[:limit])
	return out
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
	sort.SliceStable(results, func(i, j int) bool {
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
//
// 与 LookupPrefix 共享 scanPrefix；不走 hot index（hot index 不区分 exact/非 exact）。
func (r *DictReader) LookupPrefixExcludeExact(prefix string, limit int) []candidate.Candidate {
	prefix = strings.ToLower(prefix)
	if len(prefix) == 0 {
		return nil
	}
	return r.scanPrefix(prefix, limit, true)
}

// LookupPrefixBFS 广度优先（按码长分层）的前缀查找
// limitPerBucket: 每一层长度最多返回的候选数量
// maxDepth: 最大允许的剩余码长（如4码方案中输入2码，最大深度为2）
// isCommonCB: 动态回调判断字符串是否为常用字/词（由引擎层提供），
//
//	当单个 Bucket 超限时，优先保留 isCommon=true 的候选。
func (r *DictReader) LookupPrefixBFS(prefix string, limitPerBucket int, maxDepth int, isCommonCB func(text string) bool) []candidate.Candidate {
	prefix = strings.ToLower(prefix)
	if len(prefix) == 0 {
		return nil
	}

	keyCount := int(r.header.KeyCount)
	lo := sort.Search(keyCount, func(i int) bool {
		return r.readKeyCode(i) >= prefix
	})

	// 按深度 (len(code) - len(prefix)) 分配候选桶
	// depth 从 1 到 maxDepth。索引 0 对应 depth 1。
	buckets := make([][]candidate.Candidate, maxDepth)

	// 遍历以 prefix 开头的所有 code
	for i := lo; i < keyCount; i++ {
		code := r.readKeyCode(i)
		if !strings.HasPrefix(code, prefix) {
			break
		}
		if code == prefix {
			continue
		}

		depth := len(code) - len(prefix)
		if depth <= 0 || depth > maxDepth {
			continue
		}

		bucketIdx := depth - 1
		entries := r.readEntries(i)

		// 动态应用 IsCommon 回调
		if isCommonCB != nil {
			for j := range entries {
				entries[j].IsCommon = isCommonCB(entries[j].Text)
			}
		}

		buckets[bucketIdx] = append(buckets[bucketIdx], entries...)
	}

	var results []candidate.Candidate

	// 对每个 Bucket 进行截断（优先保留常用）并合并
	for _, bucket := range buckets {
		if len(bucket) == 0 {
			continue
		}

		// 先按照默认的质量排序（权重或自然顺序）
		sort.SliceStable(bucket, func(i, j int) bool {
			return candidate.Better(bucket[i], bucket[j])
		})

		// 如果该桶候选数超过限制，进行智能截断
		if limitPerBucket > 0 && len(bucket) > limitPerBucket {
			var common []candidate.Candidate
			var rare []candidate.Candidate

			for _, c := range bucket {
				if c.IsCommon {
					common = append(common, c)
				} else {
					rare = append(rare, c)
				}
			}

			var truncated []candidate.Candidate
			if len(common) >= limitPerBucket {
				// 常用字已经够填满或超出限制，只截取常用字
				truncated = common[:limitPerBucket]
			} else {
				// 常用字不够，拿生僻字凑数
				truncated = append(truncated, common...)
				needed := limitPerBucket - len(common)
				if needed > len(rare) {
					needed = len(rare)
				}
				truncated = append(truncated, rare[:needed]...)
			}
			results = append(results, truncated...)
		} else {
			results = append(results, bucket...)
		}
	}

	return results
}

// ForEachEntry 顺序遍历所有条目（供 BuildReverseIndex 等使用）
func (r *DictReader) ForEachEntry(fn func(code string, entries []candidate.Candidate)) {
	if r.isClosed() {
		return
	}
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
	if r.isClosed() {
		return -1
	}
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
	if r.isClosed() {
		return ""
	}
	off := r.keyIndexBase + uint32(i)*DictKeyIndexSize
	codeOff := byteOrder.Uint32(r.data[off : off+4])
	codeLen := byteOrder.Uint16(r.data[off+4 : off+6])
	return r.readString(codeOff, codeLen)
}

// readKeyIndex 读取第 i 个 key 的索引信息
func (r *DictReader) readKeyIndex(i int) (entryOff uint32, entryLen uint16) {
	if r.isClosed() {
		return 0, 0
	}
	off := r.keyIndexBase + uint32(i)*DictKeyIndexSize
	entryOff = byteOrder.Uint32(r.data[off+6 : off+10])
	entryLen = byteOrder.Uint16(r.data[off+10 : off+12])
	return
}

// readEntries 读取第 i 个 key 的所有候选词
func (r *DictReader) readEntries(i int) []candidate.Candidate {
	if r.isClosed() {
		return nil
	}
	code := r.readKeyCode(i)
	entryOff, entryLen := r.readKeyIndex(i)
	results := make([]candidate.Candidate, 0, entryLen)
	base := r.entryDataBase + entryOff
	recSize := r.entryRecordSize
	for j := uint16(0); j < entryLen; j++ {
		recOff := base + uint32(j)*recSize
		if recOff+recSize > uint32(len(r.data)) {
			break
		}
		textOff := byteOrder.Uint32(r.data[recOff : recOff+4])
		textLen := byteOrder.Uint16(r.data[recOff+4 : recOff+6])
		weight := int32(byteOrder.Uint32(r.data[recOff+6 : recOff+10]))

		// V3: 读取全局顺序；V2: 回退到 per-key 索引
		order := int(j)
		if recSize >= DictEntryRecordSize {
			order = int(int32(byteOrder.Uint32(r.data[recOff+10 : recOff+14])))
		}

		text := r.readString(textOff, textLen)
		results = append(results, candidate.Candidate{
			Text:         text,
			Code:         code,
			Weight:       int(weight),
			NaturalOrder: order,
		})
	}
	return results
}

// searchAbbrev 二分搜索简拼索引，返回 index 或 -1
func (r *DictReader) searchAbbrev(code string) int {
	if r.isClosed() {
		return -1
	}
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
	if r.isClosed() {
		return ""
	}
	off := r.abbrevIdxOff + uint32(i)*AbbrevIndexSize
	abbrevOff := byteOrder.Uint32(r.data[off : off+4])
	abbrevLen := byteOrder.Uint16(r.data[off+4 : off+6])
	return r.readString(abbrevOff, abbrevLen)
}

// readAbbrevEntries 读取第 i 个简拼的所有候选词
func (r *DictReader) readAbbrevEntries(i int) []candidate.Candidate {
	if r.isClosed() {
		return nil
	}
	off := r.abbrevIdxOff + uint32(i)*AbbrevIndexSize
	entryOff := byteOrder.Uint32(r.data[off+6 : off+10])
	entryLen := byteOrder.Uint16(r.data[off+10 : off+12])
	base := r.entryDataBase + entryOff
	recSize := r.entryRecordSize
	results := make([]candidate.Candidate, 0, entryLen)
	for j := uint16(0); j < entryLen; j++ {
		recOff := base + uint32(j)*recSize
		if recOff+recSize > uint32(len(r.data)) {
			break
		}
		textOff := byteOrder.Uint32(r.data[recOff : recOff+4])
		textLen := byteOrder.Uint16(r.data[recOff+4 : recOff+6])
		weight := int32(byteOrder.Uint32(r.data[recOff+6 : recOff+10]))

		order := int(j)
		if recSize >= DictEntryRecordSize {
			order = int(int32(byteOrder.Uint32(r.data[recOff+10 : recOff+14])))
		}

		text := r.readString(textOff, textLen)
		results = append(results, candidate.Candidate{
			Text:         text,
			Weight:       int(weight),
			NaturalOrder: order,
		})
	}
	return results
}
