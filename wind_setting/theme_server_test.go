package main

import (
	"bytes"
	"encoding/json"
	"net"
	"net/http"
	"net/http/httptest"
	"testing"
)

func newTestServer() *ThemeServer {
	return &ThemeServer{
		onPush: func(y string, force bool) ImportThemeResult {
			switch y {
			case "invalid":
				return ImportThemeResult{ErrorMsg: "YAML 格式错误: bad yaml"}
			case "conflict":
				if !force {
					return ImportThemeResult{Conflict: true, ThemeName: "existing-theme"}
				}
				return ImportThemeResult{Success: true, ThemeName: "existing-theme"}
			default:
				return ImportThemeResult{Success: true, ThemeName: "test-theme"}
			}
		},
		onReload: func() bool { return true },
	}
}

func TestThemeServer_PortAutoIncrement(t *testing.T) {
	ln1, err := net.Listen("tcp", "127.0.0.1:29731")
	if err != nil {
		t.Skip("端口 29731 已被占用，跳过自增测试")
	}
	defer ln1.Close()

	ts := newTestServer()
	if err := ts.Start(); err != nil {
		t.Fatalf("Start() 失败: %v", err)
	}
	defer ts.Stop()

	if ts.port == 29731 {
		t.Errorf("期望跳过 29731，实际端口: %d", ts.port)
	}
	if ts.port < 29731 || ts.port > 29733 {
		t.Errorf("端口应在 29731-29733，实际: %d", ts.port)
	}
}

func TestThemeServer_HandleListThemes(t *testing.T) {
	ts := newTestServer()
	req := httptest.NewRequest(http.MethodGet, "/api/themes", nil)
	w := httptest.NewRecorder()
	ts.handleListThemes(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("want 200, got %d: %s", w.Code, w.Body.String())
	}
	var list []themeListItem
	if err := json.NewDecoder(w.Body).Decode(&list); err != nil {
		t.Fatalf("响应应为合法 JSON 数组: %v", err)
	}
}

func TestThemeServer_HandleGetTheme_InvalidSlug(t *testing.T) {
	ts := newTestServer()
	req := httptest.NewRequest(http.MethodGet, "/api/theme/../../etc/passwd", nil)
	w := httptest.NewRecorder()
	ts.handleGetTheme(w, req)

	if w.Code != http.StatusBadRequest {
		t.Errorf("路径穿越应返回 400，got %d", w.Code)
	}
}

func TestThemeServer_HandleGetTheme_NotFound(t *testing.T) {
	ts := newTestServer()
	req := httptest.NewRequest(http.MethodGet, "/api/theme/nonexistent-theme-xyz", nil)
	w := httptest.NewRecorder()
	ts.handleGetTheme(w, req)

	if w.Code != http.StatusNotFound {
		t.Errorf("不存在主题应返回 404，got %d", w.Code)
	}
}

func TestThemeServer_HandlePushTheme_Success(t *testing.T) {
	ts := newTestServer()
	body, _ := json.Marshal(pushThemeRequest{YAML: "valid-yaml", Force: false})
	req := httptest.NewRequest(http.MethodPost, "/api/theme/push", bytes.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	w := httptest.NewRecorder()
	ts.handlePushTheme(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("want 200, got %d: %s", w.Code, w.Body.String())
	}
	var resp pushThemeResponse
	_ = json.NewDecoder(w.Body).Decode(&resp)
	if !resp.Imported {
		t.Errorf("期望 imported=true")
	}
	if !resp.Reloaded {
		t.Errorf("期望 reloaded=true（onReload 已注入）")
	}
}

func TestThemeServer_HandlePushTheme_InvalidYAML(t *testing.T) {
	ts := newTestServer()
	body, _ := json.Marshal(pushThemeRequest{YAML: "invalid", Force: false})
	req := httptest.NewRequest(http.MethodPost, "/api/theme/push", bytes.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	w := httptest.NewRecorder()
	ts.handlePushTheme(w, req)

	if w.Code != http.StatusBadRequest {
		t.Errorf("无效 YAML 应返回 400，got %d", w.Code)
	}
}

func TestThemeServer_HandlePushTheme_Conflict(t *testing.T) {
	ts := newTestServer()
	body, _ := json.Marshal(pushThemeRequest{YAML: "conflict", Force: false})
	req := httptest.NewRequest(http.MethodPost, "/api/theme/push", bytes.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	w := httptest.NewRecorder()
	ts.handlePushTheme(w, req)

	if w.Code != http.StatusConflict {
		t.Errorf("冲突应返回 409，got %d", w.Code)
	}
}

func TestThemeServer_HandlePushTheme_ForceOverwrite(t *testing.T) {
	ts := newTestServer()
	body, _ := json.Marshal(pushThemeRequest{YAML: "conflict", Force: true})
	req := httptest.NewRequest(http.MethodPost, "/api/theme/push", bytes.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	w := httptest.NewRecorder()
	ts.handlePushTheme(w, req)

	if w.Code != http.StatusOK {
		t.Errorf("force=true 应覆盖并返回 200，got %d", w.Code)
	}
}

func TestThemeServer_CORS_Preflight(t *testing.T) {
	ts := newTestServer()
	mux := http.NewServeMux()
	mux.HandleFunc("/api/themes", ts.handleListThemes)
	handler := corsMiddleware(mux)

	req := httptest.NewRequest(http.MethodOptions, "/api/themes", nil)
	req.Host = "127.0.0.1:29731"
	req.Header.Set("Origin", "https://editor.windinput.com")
	w := httptest.NewRecorder()
	handler.ServeHTTP(w, req)

	if w.Code != http.StatusNoContent {
		t.Errorf("preflight 应返回 204，got %d", w.Code)
	}
	// 白名单 Origin 应被回显（不再是 *）
	if got := w.Header().Get("Access-Control-Allow-Origin"); got != "https://editor.windinput.com" {
		t.Errorf("CORS header 应回显白名单 Origin，got %q", got)
	}
}

// TestThemeServer_CORS_RejectsNonLoopback 验证防 DNS rebinding：Host 非回环地址时拒绝。
func TestThemeServer_CORS_RejectsNonLoopback(t *testing.T) {
	ts := newTestServer()
	mux := http.NewServeMux()
	mux.HandleFunc("/api/themes", ts.handleListThemes)
	handler := corsMiddleware(mux)

	req := httptest.NewRequest(http.MethodGet, "/api/themes", nil)
	req.Host = "evil.com"
	w := httptest.NewRecorder()
	handler.ServeHTTP(w, req)

	if w.Code != http.StatusForbidden {
		t.Errorf("非回环 Host 应返回 403（防 DNS rebinding），got %d", w.Code)
	}
}

// TestThemeServer_CORS_RejectsForeignOrigin 验证非白名单 Origin 被拒绝（不执行写副作用）。
func TestThemeServer_CORS_RejectsForeignOrigin(t *testing.T) {
	ts := newTestServer()
	mux := http.NewServeMux()
	mux.HandleFunc("/api/theme/push", ts.handlePushTheme)
	handler := corsMiddleware(mux)

	req := httptest.NewRequest(http.MethodPost, "/api/theme/push", nil)
	req.Host = "127.0.0.1:29731"
	req.Header.Set("Origin", "https://evil.com")
	w := httptest.NewRecorder()
	handler.ServeHTTP(w, req)

	if w.Code != http.StatusForbidden {
		t.Errorf("非白名单 Origin 应返回 403，got %d", w.Code)
	}
}

// TestIsAllowedThemeOrigin 覆盖 Origin 白名单判定。
func TestIsAllowedThemeOrigin(t *testing.T) {
	cases := []struct {
		origin string
		want   bool
	}{
		{"https://windinput.com", true},
		{"https://editor.windinput.com", true},
		{"https://api.theme.windinput.com", true},
		{"http://localhost:5173", true},
		{"http://127.0.0.1:8080", true},
		{"https://evil.com", false},
		{"https://windinput.com.evil.com", false},
		{"https://xwindinput.com", false},
		{"http://windinput.com", false}, // 生产域要求 https
		{"", false},
	}
	for _, c := range cases {
		if got := isAllowedThemeOrigin(c.origin); got != c.want {
			t.Errorf("isAllowedThemeOrigin(%q) = %v, want %v", c.origin, got, c.want)
		}
	}
}
