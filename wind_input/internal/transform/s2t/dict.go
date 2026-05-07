package s2t

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"os"
	"sort"
)

// Dict 单个 OpenCC 词典加载后的查询结构。
//
// 内部使用一个紧凑字节池 + 排好序的 entry 数组，支持二分查找与最长前缀匹配。
// 加载后只读，可被多协程共享。
type Dict struct {
	name      string
	entries   []entry
	strings   []byte
	maxKeyLen int
}

type entry struct {
	keyOff uint32
	keyLen uint16
	valOff uint32
	valLen uint16
}

// Name 返回词典名（如 "STPhrases"）。
func (d *Dict) Name() string { return d.name }

// EntryCount 返回词条数。
func (d *Dict) EntryCount() int { return len(d.entries) }

// MaxKeyLen 返回单条 key 最大字节长度。
func (d *Dict) MaxKeyLen() int { return d.maxKeyLen }

// LoadDict 从 .octrie 文件加载词典。
func LoadDict(name, path string) (*Dict, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read %s: %w", path, err)
	}
	return ParseDict(name, data)
}

// ParseDict 从已读入的字节切片解析词典。
func ParseDict(name string, data []byte) (*Dict, error) {
	if len(data) < HeaderSize {
		return nil, fmt.Errorf("dict %s: file too small", name)
	}
	if string(data[0:4]) != FormatMagic {
		return nil, fmt.Errorf("dict %s: bad magic", name)
	}
	version := binary.LittleEndian.Uint32(data[4:8])
	if version != FormatVersion {
		return nil, fmt.Errorf("dict %s: unsupported version %d", name, version)
	}
	count := binary.LittleEndian.Uint32(data[8:12])
	maxKey := binary.LittleEndian.Uint16(data[12:14])

	entriesEnd := HeaderSize + int(count)*EntrySize
	if entriesEnd > len(data) {
		return nil, fmt.Errorf("dict %s: truncated entries", name)
	}

	entries := make([]entry, count)
	off := HeaderSize
	for i := uint32(0); i < count; i++ {
		entries[i] = entry{
			keyOff: binary.LittleEndian.Uint32(data[off : off+4]),
			keyLen: binary.LittleEndian.Uint16(data[off+4 : off+6]),
			valOff: binary.LittleEndian.Uint32(data[off+6 : off+10]),
			valLen: binary.LittleEndian.Uint16(data[off+10 : off+12]),
		}
		off += EntrySize
	}

	// StringTable 紧跟 entry 表。复制一份独立持有，避免引用大文件 buffer。
	st := make([]byte, len(data)-entriesEnd)
	copy(st, data[entriesEnd:])

	return &Dict{
		name:      name,
		entries:   entries,
		strings:   st,
		maxKeyLen: int(maxKey),
	}, nil
}

func (d *Dict) keyOf(i int) []byte {
	e := d.entries[i]
	return d.strings[e.keyOff : e.keyOff+uint32(e.keyLen)]
}

func (d *Dict) valOf(i int) []byte {
	e := d.entries[i]
	return d.strings[e.valOff : e.valOff+uint32(e.valLen)]
}

// Lookup 精确查找 key，命中返回 value（共享底层字节池，调用方不应修改）。
func (d *Dict) Lookup(key []byte) ([]byte, bool) {
	idx := sort.Search(len(d.entries), func(i int) bool {
		return bytes.Compare(d.keyOf(i), key) >= 0
	})
	if idx >= len(d.entries) {
		return nil, false
	}
	if !bytes.Equal(d.keyOf(idx), key) {
		return nil, false
	}
	return d.valOf(idx), true
}

// LongestPrefix 在 input 起始位置寻找最长的 key 命中。
// 返回命中的 keyLen 和 value 字节切片；未命中返回 (0, nil, false)。
func (d *Dict) LongestPrefix(input []byte) (int, []byte, bool) {
	if len(input) == 0 || d.maxKeyLen == 0 {
		return 0, nil, false
	}
	maxL := d.maxKeyLen
	if maxL > len(input) {
		maxL = len(input)
	}
	// 从最长字节长度开始尝试查表，命中即返回。
	// 即使 L 不在 rune 边界，input[:L] 也不会是合法词典 key（因为词典 key 都是合法 UTF-8）。
	for L := maxL; L >= 1; L-- {
		if val, ok := d.Lookup(input[:L]); ok {
			return L, val, true
		}
	}
	return 0, nil, false
}
