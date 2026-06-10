package main

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

func TestIsLoopbackHost(t *testing.T) {
	cases := []struct {
		host string
		want bool
	}{
		{"127.0.0.1:18923", true},
		{"localhost:18923", true},
		{"127.0.0.1", true},
		{"localhost", true},
		{"[::1]:18923", true},
		{"::1", true},
		{"evil.com:18923", false},
		{"example.com", false},
		{"192.168.1.10:18923", false},
		{"10.0.0.5", false},
	}
	for _, c := range cases {
		if got := isLoopbackHost(c.host); got != c.want {
			t.Errorf("isLoopbackHost(%q) = %v, want %v", c.host, got, c.want)
		}
	}
}

// serveCall 用给定 Host / Origin / Sec-Fetch-Site 发一个 POST /api/call 并返回状态码与响应体。
func serveCall(ws *webServer, host, origin, secFetchSite, body string) *httptest.ResponseRecorder {
	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodPost, "/api/call", strings.NewReader(body))
	req.Host = host
	if origin != "" {
		req.Header.Set("Origin", origin)
	}
	if secFetchSite != "" {
		req.Header.Set("Sec-Fetch-Site", secFetchSite)
	}
	ws.muxWithStatic(nil).ServeHTTP(rec, req)
	return rec
}

func TestGuard_RejectsNonLoopbackHost(t *testing.T) {
	ws := &webServer{app: &App{}, port: 18923}
	rec := serveCall(ws, "evil.com", "", "", `{"method":"GetVersion","args":[]}`)
	if rec.Code != http.StatusForbidden {
		t.Fatalf("non-loopback host: code = %d, want 403", rec.Code)
	}
}

func TestGuard_RejectsCrossSite(t *testing.T) {
	ws := &webServer{app: &App{}, port: 18923}
	rec := serveCall(ws, "127.0.0.1:18923", "", "cross-site", `{"method":"GetVersion","args":[]}`)
	if rec.Code != http.StatusForbidden {
		t.Fatalf("cross-site: code = %d, want 403", rec.Code)
	}
}

func TestGuard_RejectsCrossOrigin(t *testing.T) {
	ws := &webServer{app: &App{}, port: 18923}
	rec := serveCall(ws, "127.0.0.1:18923", "http://evil.com", "", `{"method":"GetVersion","args":[]}`)
	if rec.Code != http.StatusForbidden {
		t.Fatalf("cross-origin: code = %d, want 403", rec.Code)
	}
}

func TestGuard_AllowsSameOrigin(t *testing.T) {
	ws := &webServer{app: &App{}, port: 18923}
	rec := serveCall(ws, "127.0.0.1:18923", "http://127.0.0.1:18923", "same-origin", `{"method":"GetVersion","args":[]}`)
	if rec.Code == http.StatusForbidden {
		t.Fatalf("same-origin request should pass guard, got 403")
	}
	var resp callResponse
	if err := json.Unmarshal(rec.Body.Bytes(), &resp); err != nil {
		t.Fatalf("unmarshal: %v (body=%s)", err, rec.Body.String())
	}
	if resp.Error != "" {
		t.Fatalf("same-origin GetVersion returned error: %s", resp.Error)
	}
}

func TestWebCallDenied_RejectsDangerousMethod(t *testing.T) {
	ws := &webServer{app: &App{}, port: 18923}
	for _, method := range []string{"ResetData", "RestoreData", "ExecuteImport", "PreviewImportFile"} {
		rec := serveCall(ws, "127.0.0.1:18923", "", "same-origin",
			`{"method":"`+method+`","args":[]}`)
		var resp callResponse
		if err := json.Unmarshal(rec.Body.Bytes(), &resp); err != nil {
			t.Fatalf("%s: unmarshal: %v (body=%s)", method, err, rec.Body.String())
		}
		if resp.Error == "" {
			t.Fatalf("%s: expected denial error, got success", method)
		}
		if !strings.Contains(resp.Error, "not permitted") {
			t.Fatalf("%s: error = %q, want 'not permitted'", method, resp.Error)
		}
	}
}
