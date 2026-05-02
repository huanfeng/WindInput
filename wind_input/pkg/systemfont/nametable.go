package systemfont

import (
	"encoding/binary"
	"io"
	"os"
	"unicode/utf16"
)

// readNameTableData reads the raw bytes of the 'name' table from a font file.
// Handles both single-font TTF/OTF and TrueType Collection (TTC) files.
// Returns nil on any error or if the table is not found.
func readNameTableData(fontPath string) []byte {
	f, err := os.Open(fontPath)
	if err != nil {
		return nil
	}
	defer f.Close()

	var tag [4]byte
	if _, err := io.ReadFull(f, tag[:]); err != nil {
		return nil
	}

	if string(tag[:]) == "ttcf" {
		return readNameTableFromTTC(f)
	}

	if _, err := f.Seek(0, io.SeekStart); err != nil {
		return nil
	}
	return readNameTableFromSFNT(f)
}

// readNameTableFromTTC handles TrueType Collection files.
// Already consumed the 'ttcf' tag (4 bytes); reads the first font's name table.
func readNameTableFromTTC(r io.ReadSeeker) []byte {
	// TTC header after 'ttcf': majorVersion(2) + minorVersion(2) + numFonts(4)
	var buf [8]byte
	if _, err := io.ReadFull(r, buf[:]); err != nil {
		return nil
	}
	numFonts := binary.BigEndian.Uint32(buf[4:8])
	if numFonts == 0 {
		return nil
	}

	var offsetBuf [4]byte
	if _, err := io.ReadFull(r, offsetBuf[:]); err != nil {
		return nil
	}
	offset := binary.BigEndian.Uint32(offsetBuf[:])

	if _, err := r.Seek(int64(offset), io.SeekStart); err != nil {
		return nil
	}
	return readNameTableFromSFNT(r)
}

// readNameTableFromSFNT parses a single SFNT font (TTF or OTF) and returns
// the raw bytes of the 'name' table, capped at 64 KB.
func readNameTableFromSFNT(r io.ReadSeeker) []byte {
	var header [12]byte
	if _, err := io.ReadFull(r, header[:]); err != nil {
		return nil
	}
	numTables := binary.BigEndian.Uint16(header[4:6])

	var nameOffset, nameLength uint32
	for i := 0; i < int(numTables); i++ {
		var rec [16]byte
		if _, err := io.ReadFull(r, rec[:]); err != nil {
			return nil
		}
		if string(rec[:4]) == "name" {
			nameOffset = binary.BigEndian.Uint32(rec[8:12])
			nameLength = binary.BigEndian.Uint32(rec[12:16])
			break
		}
	}
	if nameOffset == 0 || nameLength == 0 {
		return nil
	}

	if _, err := r.Seek(int64(nameOffset), io.SeekStart); err != nil {
		return nil
	}
	if nameLength > 64*1024 {
		nameLength = 64 * 1024
	}
	data := make([]byte, nameLength)
	if _, err := io.ReadFull(r, data); err != nil {
		return nil
	}
	return data
}

// Windows-platform language IDs used to locate Chinese name records.
const (
	langZhCN = 0x0804 // Simplified Chinese
	langZhTW = 0x0404 // Traditional Chinese
)

// parseChineseFamilyName extracts the Chinese Font Family (nameID=1) from
// a raw name table. Prefers Simplified Chinese; falls back to Traditional.
func parseChineseFamilyName(data []byte) string {
	if len(data) < 6 {
		return ""
	}
	count := binary.BigEndian.Uint16(data[2:4])
	stringOffset := binary.BigEndian.Uint16(data[4:6])

	var zhTW string

	for i := 0; i < int(count); i++ {
		off := 6 + i*12
		if off+12 > len(data) {
			break
		}
		platformID := binary.BigEndian.Uint16(data[off:])
		encodingID := binary.BigEndian.Uint16(data[off+2:])
		languageID := binary.BigEndian.Uint16(data[off+4:])
		nameID := binary.BigEndian.Uint16(data[off+6:])
		length := binary.BigEndian.Uint16(data[off+8:])
		strOff := binary.BigEndian.Uint16(data[off+10:])

		if nameID != 1 || platformID != 3 || encodingID != 1 {
			continue
		}

		start := int(stringOffset) + int(strOff)
		end := start + int(length)
		if end > len(data) || length == 0 {
			continue
		}

		s := decodeUTF16BE(data[start:end])
		switch languageID {
		case langZhCN:
			return s // Best match — return immediately
		case langZhTW:
			if zhTW == "" {
				zhTW = s
			}
		}
	}

	return zhTW
}

// parseAllFamilyNames extracts every unique nameID=1 string from a raw name
// table, regardless of language ID. Results are deduplicated.
func parseAllFamilyNames(data []byte) []string {
	if len(data) < 6 {
		return nil
	}
	count := binary.BigEndian.Uint16(data[2:4])
	stringOffset := binary.BigEndian.Uint16(data[4:6])

	seen := make(map[string]struct{})
	var names []string

	for i := 0; i < int(count); i++ {
		off := 6 + i*12
		if off+12 > len(data) {
			break
		}
		platformID := binary.BigEndian.Uint16(data[off:])
		encodingID := binary.BigEndian.Uint16(data[off+2:])
		nameID := binary.BigEndian.Uint16(data[off+6:])
		length := binary.BigEndian.Uint16(data[off+8:])
		strOff := binary.BigEndian.Uint16(data[off+10:])

		// Only Windows platform (3), Unicode BMP (1), Family name (1)
		if nameID != 1 || platformID != 3 || encodingID != 1 {
			continue
		}

		start := int(stringOffset) + int(strOff)
		end := start + int(length)
		if end > len(data) || length == 0 {
			continue
		}

		s := decodeUTF16BE(data[start:end])
		if s == "" {
			continue
		}
		if _, ok := seen[s]; !ok {
			seen[s] = struct{}{}
			names = append(names, s)
		}
	}

	return names
}

// decodeUTF16BE decodes a big-endian UTF-16 byte slice into a Go string.
func decodeUTF16BE(b []byte) string {
	if len(b) < 2 || len(b)%2 != 0 {
		return ""
	}
	u16s := make([]uint16, len(b)/2)
	for i := range u16s {
		u16s[i] = binary.BigEndian.Uint16(b[i*2:])
	}
	return string(utf16.Decode(u16s))
}
