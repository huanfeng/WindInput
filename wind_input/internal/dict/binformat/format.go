package binformat

import (
	"encoding/binary"
	"fmt"
)

// 二进制词库文件格式常量

var byteOrder = binary.LittleEndian

// ---- pinyin.wdb ----

// DictMagic pinyin.wdb 文件魔数
var DictMagic = [4]byte{'W', 'D', 'I', 'C'}

const DictVersion uint32 = 1

// DictFileHeader pinyin.wdb 文件头 (32 bytes)
type DictFileHeader struct {
	Magic     [4]byte // "WDIC"
	Version   uint32  // 1
	KeyCount  uint32  // 主索引的 key 数量
	IndexOff  uint32  // KeyIndex 区偏移
	DataOff   uint32  // EntryRecords 区偏移
	StrOff    uint32  // StringPool 区偏移
	AbbrevOff uint32  // Abbrev Section 偏移 (0 表示无)
	Reserved1 uint32
}

const DictFileHeaderSize = 32

// DictKeyIndex 主索引条目 (12 bytes)
type DictKeyIndex struct {
	CodeOff  uint32 // code 字符串在 StringPool 的偏移
	CodeLen  uint16 // code 字符串长度
	EntryOff uint32 // 第一条 Entry 在 EntryRecords 区的偏移
	EntryLen uint16 // 候选数量
}

const DictKeyIndexSize = 12

// DictEntryRecord 词条记录 (10 bytes)
type DictEntryRecord struct {
	TextOff uint32 // Text 在 StringPool 的偏移
	TextLen uint16 // Text 字符串长度
	Weight  int32  // 权重
}

const DictEntryRecordSize = 10

// AbbrevHeader 简拼索引头 (16 bytes)
type AbbrevHeader struct {
	AbbrevCount  uint32
	AbbrevIdxOff uint32 // AbbrevIndex 区偏移
	Reserved1    uint32
	Reserved2    uint32
}

const AbbrevHeaderSize = 16

// AbbrevIndex 简拼索引条目 (12 bytes)
type AbbrevIndex struct {
	AbbrevOff uint32 // 简拼字符串在 StringPool 的偏移
	AbbrevLen uint16
	EntryOff  uint32 // 在 EntryRecords 中的偏移（复用主词条）
	EntryLen  uint16
}

const AbbrevIndexSize = 12

// ---- unigram.wdb ----

// UnigramMagic unigram.wdb 文件魔数
var UnigramMagic = [4]byte{'W', 'U', 'N', 'I'}

const UnigramVersion uint32 = 1

// UnigramFileHeader unigram.wdb 文件头 (24 bytes)
type UnigramFileHeader struct {
	Magic    [4]byte // "WUNI"
	Version  uint32  // 1
	KeyCount uint32
	IndexOff uint32 // KeyIndex 区偏移
	StrOff   uint32 // StringPool 区偏移
	Reserved uint32
}

const UnigramFileHeaderSize = 24

// UnigramKeyIndex unigram 索引条目 (12 bytes)
type UnigramKeyIndex struct {
	KeyOff   uint32 // key 字符串在 StringPool 的偏移
	KeyLen   uint16
	LogProb  float32 // 对数概率
	Reserved uint16
}

const UnigramKeyIndexSize = 12

// Validate 验证 DictFileHeader
func (h *DictFileHeader) Validate() error {
	if h.Magic != DictMagic {
		return fmt.Errorf("无效的词库文件魔数: %v", h.Magic)
	}
	if h.Version != DictVersion {
		return fmt.Errorf("不支持的词库版本: %d", h.Version)
	}
	return nil
}

// Validate 验证 UnigramFileHeader
func (h *UnigramFileHeader) Validate() error {
	if h.Magic != UnigramMagic {
		return fmt.Errorf("无效的 Unigram 文件魔数: %v", h.Magic)
	}
	if h.Version != UnigramVersion {
		return fmt.Errorf("不支持的 Unigram 版本: %d", h.Version)
	}
	return nil
}
