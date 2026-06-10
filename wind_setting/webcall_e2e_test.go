package main

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

// TestReflectGateway_RealAppGetVersion 端到端验证反射网关：用真实 *App 经 /api/call
// 调用 GetVersion（不依赖 rpcClient 管道），确认「method 分发 + 返回值 + 信封」全链路。
func TestReflectGateway_RealAppGetVersion(t *testing.T) {
	ws := &webServer{app: &App{}}
	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodPost, "/api/call",
		strings.NewReader(`{"method":"GetVersion","args":[]}`))
	req.Host = "127.0.0.1:18923"
	ws.muxWithStatic(nil).ServeHTTP(rec, req)

	var resp callResponse
	if err := json.Unmarshal(rec.Body.Bytes(), &resp); err != nil {
		t.Fatalf("unmarshal: %v (body=%s)", err, rec.Body.String())
	}
	if resp.Error != "" {
		t.Fatalf("unexpected error: %s", resp.Error)
	}
	s, ok := resp.Data.(string)
	if !ok || s == "" {
		t.Fatalf("expected non-empty version string, got %#v", resp.Data)
	}
	t.Logf("GetVersion via reflect gateway = %q", s)
}

// TestReflectGateway_GetPlatform 验证另一个无管道依赖方法（GetPlatform 返回 GOOS）。
func TestReflectGateway_GetPlatform(t *testing.T) {
	ws := &webServer{app: &App{}}
	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodPost, "/api/call",
		strings.NewReader(`{"method":"GetPlatform","args":[]}`))
	req.Host = "127.0.0.1:18923"
	ws.muxWithStatic(nil).ServeHTTP(rec, req)

	var resp callResponse
	if err := json.Unmarshal(rec.Body.Bytes(), &resp); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if resp.Error != "" {
		t.Fatalf("unexpected error: %s", resp.Error)
	}
	if s, ok := resp.Data.(string); !ok || s == "" {
		t.Fatalf("expected platform string, got %#v", resp.Data)
	}
}
