package rpcapi

import (
	"bytes"
	"encoding/binary"
	"io"
	"strings"
	"testing"
)

func TestWriteReadMessage_RoundTrip(t *testing.T) {
	req := Request{Version: ProtocolVersion, ID: 42, Method: "Dict.Add", Params: []byte(`{"code":"ab","text":"测试"}`)}
	var buf bytes.Buffer

	if err := WriteMessage(&buf, &req); err != nil {
		t.Fatalf("WriteMessage: %v", err)
	}

	var got Request
	if err := ReadMessage(&buf, &got); err != nil {
		t.Fatalf("ReadMessage: %v", err)
	}

	if got.Version != ProtocolVersion || got.ID != 42 || got.Method != "Dict.Add" {
		t.Errorf("unexpected: %+v", got)
	}
}

func TestWriteReadMessage_Response(t *testing.T) {
	resp := Response{ID: 1, Result: []byte(`{"total":5}`)}
	var buf bytes.Buffer

	if err := WriteMessage(&buf, &resp); err != nil {
		t.Fatal(err)
	}

	var got Response
	if err := ReadMessage(&buf, &got); err != nil {
		t.Fatal(err)
	}

	if got.ID != 1 || string(got.Result) != `{"total":5}` {
		t.Errorf("unexpected: %+v", got)
	}
}

func TestWriteReadMessage_ErrorResponse(t *testing.T) {
	resp := Response{ID: 3, Error: "store not available"}
	var buf bytes.Buffer

	WriteMessage(&buf, &resp)

	var got Response
	ReadMessage(&buf, &got)

	if got.Error != "store not available" {
		t.Errorf("expected error message, got: %q", got.Error)
	}
}

func TestWriteReadMessage_MultipleMessages(t *testing.T) {
	var buf bytes.Buffer

	for i := uint64(1); i <= 5; i++ {
		req := Request{Version: ProtocolVersion, ID: i, Method: "System.Ping"}
		if err := WriteMessage(&buf, &req); err != nil {
			t.Fatal(err)
		}
	}

	for i := uint64(1); i <= 5; i++ {
		var got Request
		if err := ReadMessage(&buf, &got); err != nil {
			t.Fatalf("read message %d: %v", i, err)
		}
		if got.ID != i {
			t.Errorf("message %d: expected ID=%d, got %d", i, i, got.ID)
		}
	}
}

func TestReadMessage_EOF(t *testing.T) {
	var buf bytes.Buffer
	var got Request
	err := ReadMessage(&buf, &got)
	if err != io.EOF {
		t.Errorf("expected io.EOF, got: %v", err)
	}
}

func TestReadMessage_TruncatedHeader(t *testing.T) {
	buf := bytes.NewReader([]byte{0, 0})
	var got Request
	err := ReadMessage(buf, &got)
	if err == nil {
		t.Error("expected error for truncated header")
	}
}

func TestReadMessage_TruncatedPayload(t *testing.T) {
	var buf bytes.Buffer
	// 写入长度头声称 100 字节，但只写 10 字节
	var header [4]byte
	binary.BigEndian.PutUint32(header[:], 100)
	buf.Write(header[:])
	buf.Write([]byte("0123456789"))

	var got Request
	err := ReadMessage(&buf, &got)
	if err == nil {
		t.Error("expected error for truncated payload")
	}
}

func TestReadMessage_TooLarge(t *testing.T) {
	var buf bytes.Buffer
	var header [4]byte
	binary.BigEndian.PutUint32(header[:], maxMessageSize+1)
	buf.Write(header[:])

	var got Request
	err := ReadMessage(&buf, &got)
	if err == nil || !strings.Contains(err.Error(), "too large") {
		t.Errorf("expected 'too large' error, got: %v", err)
	}
}

func TestWriteReadMessage_UnicodePayload(t *testing.T) {
	// 测试包含中文、换行符、特殊字符的数据不会被破坏
	req := Request{
		Version: ProtocolVersion,
		ID:      1,
		Method:  "Dict.Add",
		Params:  []byte(`{"code":"abc","text":"你好\n世界\t🌍"}`),
	}
	var buf bytes.Buffer

	if err := WriteMessage(&buf, &req); err != nil {
		t.Fatal(err)
	}

	var got Request
	if err := ReadMessage(&buf, &got); err != nil {
		t.Fatal(err)
	}

	if string(got.Params) != string(req.Params) {
		t.Errorf("params mismatch:\n  want: %s\n  got:  %s", req.Params, got.Params)
	}
}

func TestWriteReadMessage_EmptyParams(t *testing.T) {
	req := Request{Version: ProtocolVersion, ID: 1, Method: "System.Ping"}
	var buf bytes.Buffer

	WriteMessage(&buf, &req)

	var got Request
	ReadMessage(&buf, &got)

	if got.Method != "System.Ping" {
		t.Errorf("unexpected method: %s", got.Method)
	}
	// nil json.RawMessage 序列化为 "null"，反序列化后也为 "null"
	if len(got.Params) != 0 && string(got.Params) != "null" {
		t.Errorf("expected empty or null params, got: %s", got.Params)
	}
}
