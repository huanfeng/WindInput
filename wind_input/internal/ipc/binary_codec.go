package ipc

import (
	"encoding/binary"
	"errors"
	"fmt"
	"io"
)

var (
	ErrInvalidHeader  = errors.New("invalid protocol header")
	ErrVersionMismatch = errors.New("protocol version mismatch")
	ErrPayloadTooLarge = errors.New("payload too large")
)

// MaxPayloadSize is the maximum allowed payload size (1MB)
const MaxPayloadSize = 1024 * 1024

// BinaryCodec handles encoding and decoding of binary protocol messages
type BinaryCodec struct{}

// NewBinaryCodec creates a new binary codec
func NewBinaryCodec() *BinaryCodec {
	return &BinaryCodec{}
}

// ============================================================================
// Header encoding/decoding
// ============================================================================

// EncodeHeader encodes a protocol header to bytes
func (c *BinaryCodec) EncodeHeader(cmd uint16, payloadLen uint32) []byte {
	buf := make([]byte, HeaderSize)
	binary.LittleEndian.PutUint16(buf[0:2], ProtocolVersion)
	binary.LittleEndian.PutUint16(buf[2:4], cmd)
	binary.LittleEndian.PutUint32(buf[4:8], payloadLen)
	return buf
}

// DecodeHeader decodes a protocol header from bytes
func (c *BinaryCodec) DecodeHeader(buf []byte) (*IpcHeader, error) {
	if len(buf) < HeaderSize {
		return nil, ErrInvalidHeader
	}

	header := &IpcHeader{
		Version: binary.LittleEndian.Uint16(buf[0:2]),
		Command: binary.LittleEndian.Uint16(buf[2:4]),
		Length:  binary.LittleEndian.Uint32(buf[4:8]),
	}

	// Check version (only major version must match, ignore async flag)
	baseVersion := header.Version & ^AsyncFlag
	if (baseVersion >> 12) != (ProtocolVersion >> 12) {
		return nil, fmt.Errorf("%w: got %04x, expected %04x", ErrVersionMismatch, header.Version, ProtocolVersion)
	}

	if header.Length > MaxPayloadSize {
		return nil, fmt.Errorf("%w: %d bytes", ErrPayloadTooLarge, header.Length)
	}

	return header, nil
}

// ReadHeader reads and decodes a header from a reader
func (c *BinaryCodec) ReadHeader(r io.Reader) (*IpcHeader, error) {
	buf := make([]byte, HeaderSize)
	if _, err := io.ReadFull(r, buf); err != nil {
		return nil, err
	}
	return c.DecodeHeader(buf)
}

// ============================================================================
// Upstream payload decoding (C++ -> Go)
// ============================================================================

// DecodeKeyPayload decodes a key event payload
func (c *BinaryCodec) DecodeKeyPayload(buf []byte) (*KeyPayload, error) {
	if len(buf) < 16 {
		return nil, fmt.Errorf("key payload too short: %d bytes", len(buf))
	}

	return &KeyPayload{
		KeyCode:   binary.LittleEndian.Uint32(buf[0:4]),
		ScanCode:  binary.LittleEndian.Uint32(buf[4:8]),
		Modifiers: binary.LittleEndian.Uint32(buf[8:12]),
		EventType: buf[12],
	}, nil
}

// DecodeCaretPayload decodes a caret position payload
func (c *BinaryCodec) DecodeCaretPayload(buf []byte) (*CaretPayload, error) {
	if len(buf) < 12 {
		return nil, fmt.Errorf("caret payload too short: %d bytes", len(buf))
	}

	return &CaretPayload{
		X:      int32(binary.LittleEndian.Uint32(buf[0:4])),
		Y:      int32(binary.LittleEndian.Uint32(buf[4:8])),
		Height: int32(binary.LittleEndian.Uint32(buf[8:12])),
	}, nil
}

// ============================================================================
// Downstream payload encoding (Go -> C++)
// ============================================================================

// EncodeAck encodes an acknowledgment response
func (c *BinaryCodec) EncodeAck() []byte {
	return c.EncodeHeader(CmdAck, 0)
}

// EncodePassThrough encodes a pass-through response (key not handled, pass to system)
func (c *BinaryCodec) EncodePassThrough() []byte {
	return c.EncodeHeader(CmdPassThrough, 0)
}

// EncodeConsumed encodes a key consumed response
func (c *BinaryCodec) EncodeConsumed() []byte {
	return c.EncodeHeader(CmdConsumed, 0)
}

// EncodeClearComposition encodes a clear composition response
func (c *BinaryCodec) EncodeClearComposition() []byte {
	return c.EncodeHeader(CmdClearComposition, 0)
}

// EncodeCommitText encodes a commit text response
// Format: CommitTextHeader (12 bytes) + UTF-8 text + optional UTF-8 new composition
func (c *BinaryCodec) EncodeCommitText(text, newComposition string, modeChanged, chineseMode bool) []byte {
	textBytes := []byte(text)
	compBytes := []byte(newComposition)

	// Build flags
	var flags uint32
	if modeChanged {
		flags |= 0x0001 // COMMIT_FLAG_MODE_CHANGED
	}
	if len(compBytes) > 0 {
		flags |= 0x0002 // COMMIT_FLAG_HAS_NEW_COMPOSITION
	}
	if chineseMode {
		flags |= 0x0004 // COMMIT_FLAG_CHINESE_MODE
	}

	// Calculate payload size: header(12) + text + composition
	payloadLen := uint32(12 + len(textBytes) + len(compBytes))

	// Encode header
	header := c.EncodeHeader(CmdCommitText, payloadLen)

	// Encode commit header
	commitHeader := make([]byte, 12)
	binary.LittleEndian.PutUint32(commitHeader[0:4], flags)
	binary.LittleEndian.PutUint32(commitHeader[4:8], uint32(len(textBytes)))
	binary.LittleEndian.PutUint32(commitHeader[8:12], uint32(len(compBytes)))

	// Combine all parts
	result := make([]byte, 0, HeaderSize+payloadLen)
	result = append(result, header...)
	result = append(result, commitHeader...)
	result = append(result, textBytes...)
	result = append(result, compBytes...)

	return result
}

// EncodeUpdateComposition encodes an update composition response
// Format: CompositionHeader (4 bytes) + UTF-8 text
func (c *BinaryCodec) EncodeUpdateComposition(text string, caretPos int) []byte {
	textBytes := []byte(text)
	payloadLen := uint32(4 + len(textBytes))

	// Encode header
	header := c.EncodeHeader(CmdUpdateComposition, payloadLen)

	// Encode composition header
	compHeader := make([]byte, 4)
	binary.LittleEndian.PutUint32(compHeader[0:4], uint32(caretPos))

	// Combine all parts
	result := make([]byte, 0, HeaderSize+payloadLen)
	result = append(result, header...)
	result = append(result, compHeader...)
	result = append(result, textBytes...)

	return result
}

// EncodeModeChanged encodes a mode changed response
// Format: StatusFlags (4 bytes)
func (c *BinaryCodec) EncodeModeChanged(chineseMode bool) []byte {
	var flags uint32
	if chineseMode {
		flags |= StatusChineseMode
	}
	flags |= StatusModeChanged

	header := c.EncodeHeader(CmdModeChanged, 4)

	payload := make([]byte, 4)
	binary.LittleEndian.PutUint32(payload[0:4], flags)

	result := make([]byte, 0, HeaderSize+4)
	result = append(result, header...)
	result = append(result, payload...)

	return result
}

// EncodeStatusUpdate encodes a full status update response
// Format: StatusHeader (12 bytes) + keyHash values
func (c *BinaryCodec) EncodeStatusUpdate(chineseMode, fullWidth, chinesePunct, toolbarVisible, capsLock bool,
	keyDownHotkeys, keyUpHotkeys []uint32) []byte {

	// Build flags
	var flags uint32
	if chineseMode {
		flags |= StatusChineseMode
	}
	if fullWidth {
		flags |= StatusFullWidth
	}
	if chinesePunct {
		flags |= StatusChinesePunct
	}
	if toolbarVisible {
		flags |= StatusToolbarVisible
	}
	if capsLock {
		flags |= StatusCapsLock
	}

	keyDownCount := uint32(len(keyDownHotkeys))
	keyUpCount := uint32(len(keyUpHotkeys))

	// Calculate payload size: header(12) + hotkeys
	payloadLen := uint32(12 + (keyDownCount+keyUpCount)*4)

	// Encode header
	header := c.EncodeHeader(CmdStatusUpdate, payloadLen)

	// Encode status header
	statusHeader := make([]byte, 12)
	binary.LittleEndian.PutUint32(statusHeader[0:4], flags)
	binary.LittleEndian.PutUint32(statusHeader[4:8], keyDownCount)
	binary.LittleEndian.PutUint32(statusHeader[8:12], keyUpCount)

	// Encode hotkeys
	hotkeys := make([]byte, (keyDownCount+keyUpCount)*4)
	offset := 0
	for _, h := range keyDownHotkeys {
		binary.LittleEndian.PutUint32(hotkeys[offset:offset+4], h)
		offset += 4
	}
	for _, h := range keyUpHotkeys {
		binary.LittleEndian.PutUint32(hotkeys[offset:offset+4], h)
		offset += 4
	}

	// Combine all parts
	result := make([]byte, 0, HeaderSize+payloadLen)
	result = append(result, header...)
	result = append(result, statusHeader...)
	result = append(result, hotkeys...)

	return result
}

// EncodeSyncHotkeys encodes a hotkey sync message
// Format: StatusHeader (12 bytes, but only keyDownCount and keyUpCount used) + keyHash values
func (c *BinaryCodec) EncodeSyncHotkeys(keyDownHotkeys, keyUpHotkeys []uint32) []byte {
	keyDownCount := uint32(len(keyDownHotkeys))
	keyUpCount := uint32(len(keyUpHotkeys))

	// Calculate payload size: header(12) + hotkeys
	payloadLen := uint32(12 + (keyDownCount+keyUpCount)*4)

	// Encode header
	header := c.EncodeHeader(CmdSyncHotkeys, payloadLen)

	// Encode sync header (reuse StatusHeader format)
	syncHeader := make([]byte, 12)
	binary.LittleEndian.PutUint32(syncHeader[0:4], 0) // flags not used for sync
	binary.LittleEndian.PutUint32(syncHeader[4:8], keyDownCount)
	binary.LittleEndian.PutUint32(syncHeader[8:12], keyUpCount)

	// Encode hotkeys
	hotkeys := make([]byte, (keyDownCount+keyUpCount)*4)
	offset := 0
	for _, h := range keyDownHotkeys {
		binary.LittleEndian.PutUint32(hotkeys[offset:offset+4], h)
		offset += 4
	}
	for _, h := range keyUpHotkeys {
		binary.LittleEndian.PutUint32(hotkeys[offset:offset+4], h)
		offset += 4
	}

	// Combine all parts
	result := make([]byte, 0, HeaderSize+payloadLen)
	result = append(result, header...)
	result = append(result, syncHeader...)
	result = append(result, hotkeys...)

	return result
}

// WriteMessage writes a complete message (header + payload) to a writer
func (c *BinaryCodec) WriteMessage(w io.Writer, message []byte) error {
	_, err := w.Write(message)
	return err
}

// ReadPayload reads a payload of specified length from a reader
func (c *BinaryCodec) ReadPayload(r io.Reader, length uint32) ([]byte, error) {
	if length == 0 {
		return nil, nil
	}
	if length > MaxPayloadSize {
		return nil, ErrPayloadTooLarge
	}
	buf := make([]byte, length)
	if _, err := io.ReadFull(r, buf); err != nil {
		return nil, err
	}
	return buf, nil
}

// ============================================================================
// Async and Batch support
// ============================================================================

// IsAsyncRequest checks if the request has the async flag set (no response expected)
func (c *BinaryCodec) IsAsyncRequest(header *IpcHeader) bool {
	return (header.Version & AsyncFlag) != 0
}

// GetBaseVersion extracts the protocol version without the async flag
func (c *BinaryCodec) GetBaseVersion(header *IpcHeader) uint16 {
	return header.Version & ^AsyncFlag
}

// BatchEvent represents a single event within a batch
type BatchEvent struct {
	Header  *IpcHeader
	Payload []byte
	IsAsync bool // Whether this event is async (no response needed)
}

// DecodeBatchEvents decodes a batch events payload into individual events
func (c *BinaryCodec) DecodeBatchEvents(payload []byte) ([]BatchEvent, error) {
	if len(payload) < BatchHeaderSize {
		return nil, fmt.Errorf("batch payload too short: %d bytes", len(payload))
	}

	// Parse batch header
	eventCount := binary.LittleEndian.Uint16(payload[0:2])
	// reserved := binary.LittleEndian.Uint16(payload[2:4])

	events := make([]BatchEvent, 0, eventCount)
	offset := BatchHeaderSize

	for i := uint16(0); i < eventCount; i++ {
		// Check if we have enough data for a header
		if offset+HeaderSize > len(payload) {
			return nil, fmt.Errorf("batch event %d: incomplete header at offset %d", i, offset)
		}

		// Parse event header
		header, err := c.DecodeHeader(payload[offset : offset+HeaderSize])
		if err != nil {
			return nil, fmt.Errorf("batch event %d: %w", i, err)
		}
		offset += HeaderSize

		// Check if we have enough data for the payload
		if offset+int(header.Length) > len(payload) {
			return nil, fmt.Errorf("batch event %d: incomplete payload at offset %d, need %d bytes", i, offset, header.Length)
		}

		// Extract payload
		var eventPayload []byte
		if header.Length > 0 {
			eventPayload = payload[offset : offset+int(header.Length)]
			offset += int(header.Length)
		}

		events = append(events, BatchEvent{
			Header:  header,
			Payload: eventPayload,
			IsAsync: (header.Version & AsyncFlag) != 0,
		})
	}

	return events, nil
}

// EncodeBatchResponse encodes multiple responses into a batch response
func (c *BinaryCodec) EncodeBatchResponse(responses [][]byte) []byte {
	if len(responses) == 0 {
		// Return empty batch response
		header := c.EncodeHeader(CmdBatchResponse, BatchHeaderSize)
		batchHeader := make([]byte, BatchHeaderSize)
		binary.LittleEndian.PutUint16(batchHeader[0:2], 0) // responseCount = 0
		binary.LittleEndian.PutUint16(batchHeader[2:4], 0) // reserved
		return append(header, batchHeader...)
	}

	// Calculate total payload size
	totalSize := BatchHeaderSize
	for _, resp := range responses {
		totalSize += len(resp)
	}

	// Build batch header
	batchHeader := make([]byte, BatchHeaderSize)
	binary.LittleEndian.PutUint16(batchHeader[0:2], uint16(len(responses)))
	binary.LittleEndian.PutUint16(batchHeader[2:4], 0) // reserved

	// Encode outer header
	header := c.EncodeHeader(CmdBatchResponse, uint32(totalSize))

	// Combine all parts
	result := make([]byte, 0, HeaderSize+totalSize)
	result = append(result, header...)
	result = append(result, batchHeader...)
	for _, resp := range responses {
		result = append(result, resp...)
	}

	return result
}
