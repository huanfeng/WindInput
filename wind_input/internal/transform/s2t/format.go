// Package s2t 提供简体到繁体的文本转换能力（基于 OpenCC 词典数据）。
//
// 二进制 .octrie 词典格式（单文件）：
//
//	Header (16 B):
//	  Magic    [4]byte = "WIOC"
//	  Version  uint32  = 1
//	  Count    uint32  = 词条数
//	  MaxKeyB  uint16  = 单条 key 最大字节长度
//	  Reserved uint16  = 0
//	Entries (Count * 12 B), 按 keyBytes 升序，便于二分查找：
//	  KeyOff   uint32   key 在 StringTable 中的字节偏移
//	  KeyLen   uint16   key 字节长度
//	  ValOff   uint32   value 在 StringTable 中的字节偏移
//	  ValLen   uint16   value 字节长度
//	StringTable: 紧凑 UTF-8 字节池
//
// 该格式的目标：
//   - 单文件、零依赖、可 mmap（当前实现采用一次性读入）
//   - 加载即获得有序数组，支持二分查找与最长前缀匹配
//   - 内存占用 ≈ 词典原文 + 12 B/entry 元数据
package s2t

const (
	// FormatMagic .octrie 文件魔数。
	FormatMagic = "WIOC"
	// FormatVersion 当前格式版本。
	FormatVersion uint32 = 1
	// HeaderSize header 字节数。
	HeaderSize = 16
	// EntrySize 单条 entry 元数据字节数。
	EntrySize = 12
)
