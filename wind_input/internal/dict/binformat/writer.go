package binformat

import (
	"encoding/binary"
	"fmt"
	"io"
	"sort"
)

// DictEntry 写入词库时的条目
type DictEntry struct {
	Text   string
	Weight int32
}

// DictCodeEntry 一个编码对应的所有候选
type DictCodeEntry struct {
	Code    string
	Entries []DictEntry
}

// AbbrevEntry 简拼索引条目（写入时使用）
type AbbrevEntry struct {
	Abbrev  string
	Entries []DictEntry
}

// DictWriter 词库二进制格式写入器
type DictWriter struct {
	codes   []DictCodeEntry
	abbrevs []AbbrevEntry
}

// NewDictWriter 创建词库写入器
func NewDictWriter() *DictWriter {
	return &DictWriter{}
}

// AddCode 添加一个编码及其候选词
func (w *DictWriter) AddCode(code string, entries []DictEntry) {
	w.codes = append(w.codes, DictCodeEntry{Code: code, Entries: entries})
}

// AddAbbrev 添加一个简拼及其候选词
func (w *DictWriter) AddAbbrev(abbrev string, entries []DictEntry) {
	w.abbrevs = append(w.abbrevs, AbbrevEntry{Abbrev: abbrev, Entries: entries})
}

// Write 将词库写入到 writer
func (w *DictWriter) Write(out io.Writer) error {
	// 按 code 字典序排序
	sort.Slice(w.codes, func(i, j int) bool {
		return w.codes[i].Code < w.codes[j].Code
	})
	sort.Slice(w.abbrevs, func(i, j int) bool {
		return w.abbrevs[i].Abbrev < w.abbrevs[j].Abbrev
	})

	// 1. 构建 StringPool
	pool := newStringPool()
	for i := range w.codes {
		pool.Add(w.codes[i].Code)
		for j := range w.codes[i].Entries {
			pool.Add(w.codes[i].Entries[j].Text)
		}
	}
	for i := range w.abbrevs {
		pool.Add(w.abbrevs[i].Abbrev)
		for j := range w.abbrevs[i].Entries {
			pool.Add(w.abbrevs[i].Entries[j].Text)
		}
	}

	// 2. 构建 EntryRecords 和 KeyIndex
	keyCount := uint32(len(w.codes))
	keyIndexSize := keyCount * DictKeyIndexSize

	// EntryRecords: 连续排列，每个 code 的 entries 连续存放
	var entryRecords []DictEntryRecord
	type keyMeta struct {
		code     string
		entryOff uint32
		entryLen uint16
	}
	keyMetas := make([]keyMeta, 0, keyCount)
	for _, ce := range w.codes {
		off := uint32(len(entryRecords)) * DictEntryRecordSize
		for _, e := range ce.Entries {
			entryRecords = append(entryRecords, DictEntryRecord{
				TextOff: pool.Offset(e.Text),
				TextLen: uint16(len(e.Text)),
				Weight:  e.Weight,
			})
		}
		keyMetas = append(keyMetas, keyMeta{
			code:     ce.Code,
			entryOff: off,
			entryLen: uint16(len(ce.Entries)),
		})
	}
	entryRecordsSize := uint32(len(entryRecords)) * DictEntryRecordSize

	// 3. 构建 Abbrev EntryRecords 和 AbbrevIndex
	abbrevCount := uint32(len(w.abbrevs))
	var abbrevEntryRecords []DictEntryRecord
	type abbrevMeta struct {
		abbrev   string
		entryOff uint32
		entryLen uint16
	}
	abbrevMetas := make([]abbrevMeta, 0, abbrevCount)
	for _, ae := range w.abbrevs {
		off := entryRecordsSize + uint32(len(abbrevEntryRecords))*DictEntryRecordSize
		for _, e := range ae.Entries {
			abbrevEntryRecords = append(abbrevEntryRecords, DictEntryRecord{
				TextOff: pool.Offset(e.Text),
				TextLen: uint16(len(e.Text)),
				Weight:  e.Weight,
			})
		}
		abbrevMetas = append(abbrevMetas, abbrevMeta{
			abbrev:   ae.Abbrev,
			entryOff: off,
			entryLen: uint16(len(ae.Entries)),
		})
	}
	totalEntryRecordsSize := entryRecordsSize + uint32(len(abbrevEntryRecords))*DictEntryRecordSize

	// 4. 计算偏移
	indexOff := uint32(DictFileHeaderSize)
	dataOff := indexOff + keyIndexSize
	strOff := dataOff + totalEntryRecordsSize

	var abbrevOff uint32
	if abbrevCount > 0 {
		abbrevOff = strOff + pool.Size()
	}
	abbrevIdxOff := abbrevOff + AbbrevHeaderSize

	// 5. 写入 Header
	header := DictFileHeader{
		Magic:     DictMagic,
		Version:   DictVersion,
		KeyCount:  keyCount,
		IndexOff:  indexOff,
		DataOff:   dataOff,
		StrOff:    strOff,
		AbbrevOff: abbrevOff,
	}
	if err := binary.Write(out, byteOrder, header); err != nil {
		return fmt.Errorf("写入文件头失败: %w", err)
	}

	// 6. 写入 KeyIndex
	for _, km := range keyMetas {
		idx := DictKeyIndex{
			CodeOff:  pool.Offset(km.code),
			CodeLen:  uint16(len(km.code)),
			EntryOff: km.entryOff,
			EntryLen: km.entryLen,
		}
		if err := writeDictKeyIndex(out, idx); err != nil {
			return err
		}
	}

	// 7. 写入 EntryRecords (主索引部分)
	for _, er := range entryRecords {
		if err := writeDictEntryRecord(out, er); err != nil {
			return err
		}
	}
	// 写入 Abbrev 的 EntryRecords
	for _, er := range abbrevEntryRecords {
		if err := writeDictEntryRecord(out, er); err != nil {
			return err
		}
	}

	// 8. 写入 StringPool
	if _, err := out.Write(pool.Bytes()); err != nil {
		return fmt.Errorf("写入字符串池失败: %w", err)
	}

	// 9. 写入 Abbrev Section（如果有）
	if abbrevCount > 0 {
		ah := AbbrevHeader{
			AbbrevCount:  abbrevCount,
			AbbrevIdxOff: abbrevIdxOff,
		}
		if err := binary.Write(out, byteOrder, ah); err != nil {
			return fmt.Errorf("写入简拼头失败: %w", err)
		}

		for _, am := range abbrevMetas {
			ai := AbbrevIndex{
				AbbrevOff: pool.Offset(am.abbrev),
				AbbrevLen: uint16(len(am.abbrev)),
				EntryOff:  am.entryOff,
				EntryLen:  am.entryLen,
			}
			if err := writeAbbrevIndex(out, ai); err != nil {
				return err
			}
		}
	}

	return nil
}

// writeDictKeyIndex 写入 KeyIndex 条目（12 bytes 紧凑格式）
func writeDictKeyIndex(w io.Writer, idx DictKeyIndex) error {
	var buf [DictKeyIndexSize]byte
	byteOrder.PutUint32(buf[0:4], idx.CodeOff)
	byteOrder.PutUint16(buf[4:6], idx.CodeLen)
	byteOrder.PutUint32(buf[6:10], idx.EntryOff)
	byteOrder.PutUint16(buf[10:12], idx.EntryLen)
	_, err := w.Write(buf[:])
	return err
}

// writeDictEntryRecord 写入 EntryRecord（10 bytes）
func writeDictEntryRecord(w io.Writer, er DictEntryRecord) error {
	var buf [DictEntryRecordSize]byte
	byteOrder.PutUint32(buf[0:4], er.TextOff)
	byteOrder.PutUint16(buf[4:6], er.TextLen)
	byteOrder.PutUint32(buf[6:10], uint32(er.Weight))
	_, err := w.Write(buf[:])
	return err
}

// writeAbbrevIndex 写入 AbbrevIndex（12 bytes）
func writeAbbrevIndex(w io.Writer, ai AbbrevIndex) error {
	var buf [AbbrevIndexSize]byte
	byteOrder.PutUint32(buf[0:4], ai.AbbrevOff)
	byteOrder.PutUint16(buf[4:6], ai.AbbrevLen)
	byteOrder.PutUint32(buf[6:10], ai.EntryOff)
	byteOrder.PutUint16(buf[10:12], ai.EntryLen)
	_, err := w.Write(buf[:])
	return err
}

// stringPool 字符串池构建器
type stringPool struct {
	buf     []byte
	offsets map[string]uint32
}

func newStringPool() *stringPool {
	return &stringPool{
		offsets: make(map[string]uint32),
	}
}

func (p *stringPool) Add(s string) {
	if _, ok := p.offsets[s]; ok {
		return
	}
	p.offsets[s] = uint32(len(p.buf))
	p.buf = append(p.buf, []byte(s)...)
}

func (p *stringPool) Offset(s string) uint32 {
	return p.offsets[s]
}

func (p *stringPool) Size() uint32 {
	return uint32(len(p.buf))
}

func (p *stringPool) Bytes() []byte {
	return p.buf
}
