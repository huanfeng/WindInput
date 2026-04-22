package datformat

import "fmt"

// WdatMagic wdat 文件魔数
var WdatMagic = [4]byte{'W', 'D', 'A', 'T'}

// WdatVersion wdat 文件版本
// v1: 原始格式，DAT 使用原始字节码
// v2: DAT 使用紧凑字符映射，文件头后附加 CharMap 区段
const WdatVersion uint32 = 2

// WdatVersionV1 v1 版本号，用于兼容读取
const WdatVersionV1 uint32 = 1

// WdatFileHeader wdat 文件头 (48 bytes, little-endian)
type WdatFileHeader struct {
	Magic      [4]byte // "WDAT"
	Version    uint32  // 1
	DATSize    uint32  // DAT 数组元素个数
	LeafCount  uint32  // 叶节点数量 (= key 数量)
	DATOff     uint32  // DAT Base 数组偏移
	LeafOff    uint32  // LeafTable 偏移
	EntryOff   uint32  // EntryRecords 偏移
	StrOff     uint32  // StringPool 偏移
	AbbrevOff  uint32  // AbbrevSection 偏移 (0=无)
	MetaOff    uint32  // Meta 偏移 (0=无)
	EntryCount uint32  // EntryRecords 总记录数
	CharMapOff uint32  // v2: CharMap 区段偏移 (0=无，v1 兼容)
}

// WdatFileHeaderSize WdatFileHeader 的字节大小
const WdatFileHeaderSize = 48

// LeafRecord 叶节点记录 (8 bytes)
type LeafRecord struct {
	EntryOff uint32 // 第一条 Entry 在 EntryRecords 的字节偏移
	EntryLen uint16 // 候选词数量
	Reserved uint16
}

// LeafRecordSize LeafRecord 的字节大小
const LeafRecordSize = 8

// EntryRecord 词条记录 (10 bytes)
type EntryRecord struct {
	TextOff uint32
	TextLen uint16
	Weight  int32
}

// EntryRecordSize EntryRecord 的字节大小
const EntryRecordSize = 10

// CharMapSection 字符映射区段 (1028 bytes)
type CharMapSection struct {
	MaxCode int32      // 最大 compact code 值
	CharMap [256]int32 // byte → compact code (-1 = unmapped)
}

// CharMapSectionSize CharMapSection 的字节大小
const CharMapSectionSize = 4 + 256*4 // 1028 bytes

// AbbrevSection 简拼区段头 (16 bytes)
type AbbrevSection struct {
	DATSize   uint32
	LeafCount uint32
	DATOff    uint32 // 简拼 DAT 偏移（相对文件开头）
	LeafOff   uint32 // 简拼 LeafTable 偏移
}

// AbbrevSectionSize AbbrevSection 的字节大小
const AbbrevSectionSize = 16

// Validate 校验文件头合法性
func (h *WdatFileHeader) Validate() error {
	if h.Magic != WdatMagic {
		return fmt.Errorf("invalid magic: %q", h.Magic)
	}
	if h.Version != WdatVersion && h.Version != WdatVersionV1 {
		return fmt.Errorf("unsupported version: %d", h.Version)
	}
	if h.DATOff < WdatFileHeaderSize {
		return fmt.Errorf("DATOff %d < header size %d", h.DATOff, WdatFileHeaderSize)
	}
	return nil
}
