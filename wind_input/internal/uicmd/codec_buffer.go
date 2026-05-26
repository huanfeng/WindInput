package uicmd

import (
	"encoding/binary"
	"errors"
	"math"
)

// binWriter / binReader 提供小端字节流读写工具, 内部 panic-free,
// 错误通过返回值传递。字符串/切片统一用 uint32 长度前缀。
// 编码风格与 internal/ipc/binary_codec.go 一致, 便于 IMKit/wind_tsf 端对称解码。

var (
	errBufUnderflow  = errors.New("uicmd: buffer underflow")
	errStringTooLong = errors.New("uicmd: string length exceeds uint32")
	errSliceTooLong  = errors.New("uicmd: slice length exceeds uint32")
)

type binWriter struct {
	buf []byte
}

func newBinWriter(cap int) *binWriter {
	return &binWriter{buf: make([]byte, 0, cap)}
}

func (w *binWriter) Bytes() []byte { return w.buf }

func (w *binWriter) writeU8(v uint8) {
	w.buf = append(w.buf, v)
}

func (w *binWriter) writeBool(v bool) {
	if v {
		w.buf = append(w.buf, 1)
	} else {
		w.buf = append(w.buf, 0)
	}
}

func (w *binWriter) writeU16(v uint16) {
	var b [2]byte
	binary.LittleEndian.PutUint16(b[:], v)
	w.buf = append(w.buf, b[:]...)
}

func (w *binWriter) writeU32(v uint32) {
	var b [4]byte
	binary.LittleEndian.PutUint32(b[:], v)
	w.buf = append(w.buf, b[:]...)
}

func (w *binWriter) writeI32(v int32) {
	w.writeU32(uint32(v))
}

func (w *binWriter) writeU64(v uint64) {
	var b [8]byte
	binary.LittleEndian.PutUint64(b[:], v)
	w.buf = append(w.buf, b[:]...)
}

func (w *binWriter) writeF64(v float64) {
	w.writeU64(math.Float64bits(v))
}

func (w *binWriter) writeString(s string) error {
	if len(s) > math.MaxUint32 {
		return errStringTooLong
	}
	w.writeU32(uint32(len(s)))
	w.buf = append(w.buf, s...)
	return nil
}

// writeColor 写入 4 字节 RGBA。
func (w *binWriter) writeColor(c Color) {
	w.writeU8(c.R)
	w.writeU8(c.G)
	w.writeU8(c.B)
	w.writeU8(c.A)
}

// writeOptColor 写入 nullable Color: [1 字节 present][RGBA?]。
func (w *binWriter) writeOptColor(c *Color) {
	if c == nil {
		w.writeU8(0)
		return
	}
	w.writeU8(1)
	w.writeColor(*c)
}

type binReader struct {
	buf []byte
	pos int
}

func newBinReader(buf []byte) *binReader {
	return &binReader{buf: buf}
}

func (r *binReader) remaining() int { return len(r.buf) - r.pos }

func (r *binReader) eof() bool { return r.pos >= len(r.buf) }

func (r *binReader) readU8() (uint8, error) {
	if r.remaining() < 1 {
		return 0, errBufUnderflow
	}
	v := r.buf[r.pos]
	r.pos++
	return v, nil
}

func (r *binReader) readBool() (bool, error) {
	v, err := r.readU8()
	if err != nil {
		return false, err
	}
	return v != 0, nil
}

func (r *binReader) readU16() (uint16, error) {
	if r.remaining() < 2 {
		return 0, errBufUnderflow
	}
	v := binary.LittleEndian.Uint16(r.buf[r.pos:])
	r.pos += 2
	return v, nil
}

func (r *binReader) readU32() (uint32, error) {
	if r.remaining() < 4 {
		return 0, errBufUnderflow
	}
	v := binary.LittleEndian.Uint32(r.buf[r.pos:])
	r.pos += 4
	return v, nil
}

func (r *binReader) readI32() (int32, error) {
	v, err := r.readU32()
	return int32(v), err
}

func (r *binReader) readU64() (uint64, error) {
	if r.remaining() < 8 {
		return 0, errBufUnderflow
	}
	v := binary.LittleEndian.Uint64(r.buf[r.pos:])
	r.pos += 8
	return v, nil
}

func (r *binReader) readF64() (float64, error) {
	v, err := r.readU64()
	if err != nil {
		return 0, err
	}
	return math.Float64frombits(v), nil
}

func (r *binReader) readString() (string, error) {
	n, err := r.readU32()
	if err != nil {
		return "", err
	}
	if r.remaining() < int(n) {
		return "", errBufUnderflow
	}
	s := string(r.buf[r.pos : r.pos+int(n)])
	r.pos += int(n)
	return s, nil
}

func (r *binReader) readColor() (Color, error) {
	if r.remaining() < 4 {
		return Color{}, errBufUnderflow
	}
	c := Color{R: r.buf[r.pos], G: r.buf[r.pos+1], B: r.buf[r.pos+2], A: r.buf[r.pos+3]}
	r.pos += 4
	return c, nil
}

func (r *binReader) readOptColor() (*Color, error) {
	present, err := r.readU8()
	if err != nil {
		return nil, err
	}
	if present == 0 {
		return nil, nil
	}
	c, err := r.readColor()
	if err != nil {
		return nil, err
	}
	return &c, nil
}
