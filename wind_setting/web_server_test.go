package main

import (
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

type fakeTarget struct{}

func (fakeTarget) Echo(s string) string  { return "hi:" + s }
func (fakeTarget) Add(a, b int) int      { return a + b }
func (fakeTarget) Boom() (string, error) { return "", errors.New("boom") }
func (fakeTarget) Ok() (string, error)   { return "ok", nil }
func (fakeTarget) Noop()                 {}
func (fakeTarget) Panic() string         { panic("kaboom") }

func raw(s string) json.RawMessage { return json.RawMessage(s) }

func TestCallReflect_StringArg(t *testing.T) {
	data, err := callReflect(fakeTarget{}, "Echo", []json.RawMessage{raw(`"x"`)})
	if err != nil {
		t.Fatalf("err = %v", err)
	}
	if data != "hi:x" {
		t.Fatalf("data = %v, want hi:x", data)
	}
}

func TestCallReflect_TwoIntArgs(t *testing.T) {
	data, err := callReflect(fakeTarget{}, "Add", []json.RawMessage{raw(`2`), raw(`3`)})
	if err != nil || data != 5 {
		t.Fatalf("data=%v err=%v, want 5 nil", data, err)
	}
}

func TestCallReflect_ErrorReturn(t *testing.T) {
	_, err := callReflect(fakeTarget{}, "Boom", nil)
	if err == nil || err.Error() != "boom" {
		t.Fatalf("err = %v, want boom", err)
	}
}

func TestCallReflect_DataWithNilError(t *testing.T) {
	data, err := callReflect(fakeTarget{}, "Ok", nil)
	if err != nil || data != "ok" {
		t.Fatalf("data=%v err=%v, want ok nil", data, err)
	}
}

func TestCallReflect_UnknownMethod(t *testing.T) {
	_, err := callReflect(fakeTarget{}, "Nope", nil)
	if err == nil {
		t.Fatalf("want error for unknown method")
	}
}

func TestCallReflect_ArgCountMismatch(t *testing.T) {
	_, err := callReflect(fakeTarget{}, "Add", []json.RawMessage{raw(`1`)})
	if err == nil {
		t.Fatalf("want error for arg count mismatch")
	}
}

func TestCallReflect_VoidMethod(t *testing.T) {
	data, err := callReflect(fakeTarget{}, "Noop", nil)
	if err != nil || data != nil {
		t.Fatalf("data=%v err=%v, want nil nil", data, err)
	}
}

func TestCallReflect_RecoversPanic(t *testing.T) {
	_, err := callReflect(fakeTarget{}, "Panic", nil)
	if err == nil {
		t.Fatalf("want error from panicking method, got nil")
	}
}

func TestCallEndpoint_MethodGuard(t *testing.T) {
	ws := &webServer{}
	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodGet, "/api/call", nil)
	req.Host = "127.0.0.1:18923"
	ws.muxWithStatic(nil).ServeHTTP(rec, req)
	var resp callResponse
	_ = json.Unmarshal(rec.Body.Bytes(), &resp)
	if resp.Error == "" {
		t.Fatalf("want error in body for GET /api/call")
	}
}

func TestCallEndpoint_UnknownMethod(t *testing.T) {
	ws := &webServer{app: &App{}}
	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodPost, "/api/call", strings.NewReader(`{"method":"NoSuchMethod","args":[]}`))
	req.Host = "127.0.0.1:18923"
	ws.muxWithStatic(nil).ServeHTTP(rec, req)
	var resp callResponse
	if err := json.Unmarshal(rec.Body.Bytes(), &resp); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if resp.Error == "" {
		t.Fatalf("want error for unknown method")
	}
}

func TestNoCORSHeader(t *testing.T) {
	ws := &webServer{}
	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodGet, "/api/call", nil)
	req.Host = "127.0.0.1:18923"
	ws.muxWithStatic(nil).ServeHTTP(rec, req)
	if got := rec.Header().Get("Access-Control-Allow-Origin"); got != "" {
		t.Fatalf("Access-Control-Allow-Origin = %q, want empty (no CORS)", got)
	}
}

func TestStartPicksFreePort(t *testing.T) {
	ws := &webServer{}
	if err := ws.Start(nil); err != nil {
		t.Fatalf("Start: %v", err)
	}
	defer ws.Stop()
	if ws.port < webServerBasePort || ws.port >= webServerBasePort+webServerMaxTries {
		t.Fatalf("port = %d out of range", ws.port)
	}
}

// withShortIdle 临时把空闲退出参数调小，便于快速测试。
func withShortIdle(t *testing.T) {
	t.Helper()
	oTimeout, oCheck, oBye := webIdleTimeout, webIdleCheck, webByeGrace
	webIdleTimeout = 60 * time.Millisecond
	webIdleCheck = 10 * time.Millisecond
	webByeGrace = 20 * time.Millisecond
	t.Cleanup(func() { webIdleTimeout, webIdleCheck, webByeGrace = oTimeout, oCheck, oBye })
}

// TestIdleAutoShutdown 验证：启动后若一直无心跳 ping，Wait() 通道关闭（自动退出）。
func TestIdleAutoShutdown(t *testing.T) {
	withShortIdle(t)
	ws := &webServer{}
	if err := ws.Start(nil); err != nil {
		t.Fatalf("Start: %v", err)
	}
	defer ws.Stop()

	select {
	case <-ws.Wait():
		// 期望：超时无心跳后自动退出
	case <-time.After(2 * time.Second):
		t.Fatalf("Wait() 未在空闲超时后关闭")
	}
}

// TestPingKeepsAlive 验证：持续心跳 ping 时不会误退出。
func TestPingKeepsAlive(t *testing.T) {
	withShortIdle(t)
	ws := &webServer{}
	if err := ws.Start(nil); err != nil {
		t.Fatalf("Start: %v", err)
	}
	defer ws.Stop()

	// 在超过 webIdleTimeout 的总时长内，按短于超时的间隔持续 ping。
	stop := make(chan struct{})
	defer close(stop)
	go func() {
		tk := time.NewTicker(15 * time.Millisecond)
		defer tk.Stop()
		for {
			select {
			case <-stop:
				return
			case <-tk.C:
				ws.touch()
			}
		}
	}()

	select {
	case <-ws.Wait():
		t.Fatalf("持续心跳期间不应自动退出")
	case <-time.After(200 * time.Millisecond):
		// 期望：心跳维持存活
	}
}

// TestByeTriggersShutdown 验证：收到关闭信标且其后无 ping，则在 byeGrace 后退出。
func TestByeTriggersShutdown(t *testing.T) {
	withShortIdle(t)
	ws := &webServer{}
	if err := ws.Start(nil); err != nil {
		t.Fatalf("Start: %v", err)
	}
	defer ws.Stop()

	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodPost, "/api/bye", nil)
	req.Host = "127.0.0.1:18923"
	ws.muxWithStatic(nil).ServeHTTP(rec, req)

	select {
	case <-ws.Wait():
		// 期望：byeGrace 后退出
	case <-time.After(2 * time.Second):
		t.Fatalf("收到 /api/bye 后未退出")
	}
}
