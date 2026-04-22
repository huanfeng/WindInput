package datformat

import (
	"encoding/binary"
	"io"
	"sort"
)

// stringPool 字符串池，去重并追踪偏移
type stringPool struct {
	buf   []byte
	index map[string]uint32
}

func newStringPool() *stringPool {
	return &stringPool{index: make(map[string]uint32)}
}

// add 将 s 加入字符串池，返回其偏移量
func (p *stringPool) add(s string) uint32 {
	if off, ok := p.index[s]; ok {
		return off
	}
	off := uint32(len(p.buf))
	p.buf = append(p.buf, []byte(s)...)
	p.index[s] = off
	return off
}

// WdatEntry 单条词条
type WdatEntry struct {
	Text   string
	Weight int32
}

// WdatCodeEntry 全拼编码词条组
type WdatCodeEntry struct {
	Code    string
	Entries []WdatEntry
}

// WdatAbbrevEntry 简拼编码词条组
type WdatAbbrevEntry struct {
	Abbrev  string
	Entries []WdatEntry
}

// WdatWriter wdat 文件写入器
type WdatWriter struct {
	codes   []WdatCodeEntry
	abbrevs []WdatAbbrevEntry
	meta    []byte
}

// NewWdatWriter 创建新的写入器
func NewWdatWriter() *WdatWriter {
	return &WdatWriter{}
}

// AddCode 添加全拼编码词条组
func (w *WdatWriter) AddCode(code string, entries []WdatEntry) {
	w.codes = append(w.codes, WdatCodeEntry{Code: code, Entries: entries})
}

// AddAbbrev 添加简拼编码词条组
func (w *WdatWriter) AddAbbrev(abbrev string, entries []WdatEntry) {
	w.abbrevs = append(w.abbrevs, WdatAbbrevEntry{Abbrev: abbrev, Entries: entries})
}

// SetMeta 设置 JSON 元数据
func (w *WdatWriter) SetMeta(data []byte) {
	w.meta = data
}

// buildEntriesAndLeaves 构建字符串池、EntryRecords、LeafTable，返回相关切片
// strPool 是共享的字符串池
// entries 以字节为单位相对于所在 EntryRecords 区块起始的偏移
func buildEntriesAndLeaves(codes []WdatCodeEntry, pool *stringPool) ([]LeafRecord, []EntryRecord) {
	leaves := make([]LeafRecord, len(codes))
	var allEntries []EntryRecord
	entryByteOff := uint32(0)
	for i, ce := range codes {
		leaves[i] = LeafRecord{
			EntryOff: entryByteOff,
			EntryLen: uint16(len(ce.Entries)),
		}
		for _, e := range ce.Entries {
			textOff := pool.add(e.Text)
			allEntries = append(allEntries, EntryRecord{
				TextOff: textOff,
				TextLen: uint16(len(e.Text)),
				Weight:  e.Weight,
			})
		}
		entryByteOff += uint32(len(ce.Entries)) * EntryRecordSize
	}
	return leaves, allEntries
}

// writeEntryRecord 写入单条 EntryRecord（小端序，10 bytes）
func writeEntryRecord(w io.Writer, e EntryRecord) error {
	if err := binary.Write(w, byteOrder, e.TextOff); err != nil {
		return err
	}
	if err := binary.Write(w, byteOrder, e.TextLen); err != nil {
		return err
	}
	return binary.Write(w, byteOrder, e.Weight)
}

// Write 将数据序列化写入 out
func (w *WdatWriter) Write(out io.Writer) error {
	// 1. 排序
	sort.Slice(w.codes, func(i, j int) bool { return w.codes[i].Code < w.codes[j].Code })
	sort.Slice(w.abbrevs, func(i, j int) bool { return w.abbrevs[i].Abbrev < w.abbrevs[j].Abbrev })

	// 2. 构建共享字符串池（主 codes + abbrevs 共用同一个 pool）
	pool := newStringPool()
	for _, ce := range w.codes {
		for _, e := range ce.Entries {
			pool.add(e.Text)
		}
	}
	for _, ae := range w.abbrevs {
		for _, e := range ae.Entries {
			pool.add(e.Text)
		}
	}

	// 3. 构建主 EntryRecords + LeafTable
	mainLeaves, mainEntries := buildEntriesAndLeaves(w.codes, pool)

	// 4. 构建主 DAT
	mainDATBuilder := NewDATBuilder()
	for i, ce := range w.codes {
		mainDATBuilder.Add(ce.Code, uint32(i))
	}
	mainDAT, err := mainDATBuilder.Build()
	if err != nil {
		return err
	}

	// 5. 简拼
	var abbrevLeaves []LeafRecord
	var abbrevEntries []EntryRecord
	var abbrevDAT *DAT
	hasAbbrev := len(w.abbrevs) > 0

	if hasAbbrev {
		abbrevCodes := make([]WdatCodeEntry, len(w.abbrevs))
		for i, ae := range w.abbrevs {
			abbrevCodes[i] = WdatCodeEntry{Code: ae.Abbrev, Entries: ae.Entries}
		}
		abbrevLeaves, abbrevEntries = buildEntriesAndLeaves(abbrevCodes, pool)

		abbrevDATBuilder := NewDATBuilder()
		for i, ae := range w.abbrevs {
			abbrevDATBuilder.Add(ae.Abbrev, uint32(i))
		}
		abbrevDAT, err = abbrevDATBuilder.Build()
		if err != nil {
			return err
		}
	}

	// 6. 计算各区偏移
	// Header
	off := uint32(WdatFileHeaderSize)

	// DAT Base + Check (主)
	mainDATOff := off
	mainDATBytes := uint32(mainDAT.Size) * 4 * 2 // base + check, each int32
	off += mainDATBytes

	// LeafTable (主)
	mainLeafOff := off
	off += uint32(len(mainLeaves)) * LeafRecordSize

	// EntryRecords (主)
	mainEntryOff := off
	off += uint32(len(mainEntries)) * EntryRecordSize

	// StringPool
	strOff := off
	off += uint32(len(pool.buf))

	// AbbrevSection
	abbrevOff := uint32(0)
	if hasAbbrev {
		abbrevOff = off
		// AbbrevSection header
		off += AbbrevSectionSize
		// Abbrev DAT
		abbrevDATOff := off
		off += uint32(abbrevDAT.Size) * 4 * 2
		// Abbrev LeafTable
		abbrevLeafOff := off
		off += uint32(len(abbrevLeaves)) * LeafRecordSize
		// Abbrev EntryRecords
		off += uint32(len(abbrevEntries)) * EntryRecordSize
		// Abbrev CharMap
		off += CharMapSectionSize

		// 更新 abbrevLeaves 中的 EntryOff 已经是相对于本区块起始的字节偏移，保持不变
		_ = abbrevDATOff
		_ = abbrevLeafOff
	}

	// CharMap 区段（主 DAT）
	charMapOff := off
	off += CharMapSectionSize

	// Meta
	metaOff := uint32(0)
	if len(w.meta) > 0 {
		metaOff = off
	}

	// 7. 写入文件头
	hdr := WdatFileHeader{
		Magic:      WdatMagic,
		Version:    WdatVersion,
		DATSize:    uint32(mainDAT.Size),
		LeafCount:  uint32(len(mainLeaves)),
		DATOff:     mainDATOff,
		LeafOff:    mainLeafOff,
		EntryOff:   mainEntryOff,
		StrOff:     strOff,
		AbbrevOff:  abbrevOff,
		MetaOff:    metaOff,
		EntryCount: uint32(len(mainEntries)),
		CharMapOff: charMapOff,
	}
	if err := binary.Write(out, byteOrder, hdr); err != nil {
		return err
	}

	// 8. 写入主 DAT Base 数组
	for _, v := range mainDAT.Base {
		if err := binary.Write(out, byteOrder, v); err != nil {
			return err
		}
	}
	// 写入主 DAT Check 数组
	for _, v := range mainDAT.Check {
		if err := binary.Write(out, byteOrder, v); err != nil {
			return err
		}
	}

	// 9. 写入主 LeafTable
	for _, lr := range mainLeaves {
		if err := binary.Write(out, byteOrder, lr); err != nil {
			return err
		}
	}

	// 10. 写入主 EntryRecords
	for _, er := range mainEntries {
		if err := writeEntryRecord(out, er); err != nil {
			return err
		}
	}

	// 11. 写入 StringPool
	if _, err := out.Write(pool.buf); err != nil {
		return err
	}

	// 12. 写入 AbbrevSection
	if hasAbbrev {
		// 计算简拼各区绝对偏移
		abbrevSectionStart := abbrevOff
		abbrevDATAbsOff := abbrevSectionStart + AbbrevSectionSize
		abbrevLeafAbsOff := abbrevDATAbsOff + uint32(abbrevDAT.Size)*4*2

		abbrevHdr := AbbrevSection{
			DATSize:   uint32(abbrevDAT.Size),
			LeafCount: uint32(len(abbrevLeaves)),
			DATOff:    abbrevDATAbsOff,
			LeafOff:   abbrevLeafAbsOff,
		}
		if err := binary.Write(out, byteOrder, abbrevHdr); err != nil {
			return err
		}

		// Abbrev DAT Base
		for _, v := range abbrevDAT.Base {
			if err := binary.Write(out, byteOrder, v); err != nil {
				return err
			}
		}
		// Abbrev DAT Check
		for _, v := range abbrevDAT.Check {
			if err := binary.Write(out, byteOrder, v); err != nil {
				return err
			}
		}

		// Abbrev LeafTable
		for _, lr := range abbrevLeaves {
			if err := binary.Write(out, byteOrder, lr); err != nil {
				return err
			}
		}

		// Abbrev EntryRecords
		for _, er := range abbrevEntries {
			if err := writeEntryRecord(out, er); err != nil {
				return err
			}
		}

		// Abbrev CharMap
		abbrevCharMapSection := CharMapSection{
			MaxCode: abbrevDAT.MaxCode,
			CharMap: abbrevDAT.CharMap,
		}
		if err := binary.Write(out, byteOrder, abbrevCharMapSection); err != nil {
			return err
		}
	}

	// 13. 写入 CharMap 区段
	charMapSection := CharMapSection{
		MaxCode: mainDAT.MaxCode,
		CharMap: mainDAT.CharMap,
	}
	if err := binary.Write(out, byteOrder, charMapSection); err != nil {
		return err
	}

	// 14. 写入 Meta
	if len(w.meta) > 0 {
		metaLen := uint32(len(w.meta))
		if err := binary.Write(out, byteOrder, metaLen); err != nil {
			return err
		}
		if _, err := out.Write(w.meta); err != nil {
			return err
		}
	}

	return nil
}
