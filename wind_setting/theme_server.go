package main

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/theme"
)

const (
	themeServerBasePort = 29731
	themeServerMaxTries = 3
)

// ThemeServerStatus 服务状态（Wails 导出 + 前端展示）
type ThemeServerStatus struct {
	Running bool   `json:"running"`
	Port    int    `json:"port"`
	URL     string `json:"url"`
}

// ThemeServer 本地 HTTP 服务，供 Web 编辑器连接推送/拉取主题。
// onPush/onReload 通过函数字段注入，使 handler 层与 Wails 运行时解耦。
type ThemeServer struct {
	onPush   func(yamlContent string, force bool) ImportThemeResult
	onReload func() bool
	server   *http.Server
	port     int
}

// Start 从 themeServerBasePort 起尝试绑定，最多 themeServerMaxTries 次。
func (ts *ThemeServer) Start() error {
	mux := http.NewServeMux()
	mux.HandleFunc("/api/themes", ts.handleListThemes)
	mux.HandleFunc("/api/theme/push", ts.handlePushTheme)
	mux.HandleFunc("/api/theme/", ts.handleGetTheme)

	handler := corsMiddleware(mux)

	var lastErr error
	for i := range themeServerMaxTries {
		port := themeServerBasePort + i
		ln, err := net.Listen("tcp", "127.0.0.1:"+strconv.Itoa(port))
		if err != nil {
			lastErr = err
			continue
		}
		ts.port = port
		srv := &http.Server{Handler: handler}
		ts.server = srv
		go func() { _ = srv.Serve(ln) }()
		return nil
	}
	return fmt.Errorf("端口 %d-%d 均被占用：%w",
		themeServerBasePort, themeServerBasePort+themeServerMaxTries-1, lastErr)
}

// Stop 优雅关闭（3s 超时）。
func (ts *ThemeServer) Stop() {
	if ts.server == nil {
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	_ = ts.server.Shutdown(ctx)
	ts.server = nil
	ts.port = 0
}

// Status 返回当前运行状态。
func (ts *ThemeServer) Status() ThemeServerStatus {
	if ts.server == nil || ts.port == 0 {
		return ThemeServerStatus{}
	}
	return ThemeServerStatus{
		Running: true,
		Port:    ts.port,
		URL:     "http://localhost:" + strconv.Itoa(ts.port),
	}
}

// corsMiddleware 为外部主题编辑器（独立仓 WindInputThemeEditor，跨域）开放 CORS，
// 但加两道约束（详见 web_security.go）：
//   - Host 必须指向本机回环地址，抵御 DNS rebinding。
//   - Origin（若存在）必须在白名单内（*.windinput.com 或 localhost），否则直接 403，
//     连写副作用（push 主题 + reload）都不执行；并按请求回显具体 Origin（不再用 *）。
//
// 无 Origin 的请求（同源页面或非浏览器工具）放行，由上面的 Host 回环校验兜底。
func corsMiddleware(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !isLoopbackHost(r.Host) {
			http.Error(w, "forbidden: non-loopback host", http.StatusForbidden)
			return
		}
		if origin := r.Header.Get("Origin"); origin != "" {
			if !isAllowedThemeOrigin(origin) {
				http.Error(w, "forbidden: origin not allowed", http.StatusForbidden)
				return
			}
			w.Header().Set("Access-Control-Allow-Origin", origin)
			w.Header().Add("Vary", "Origin")
		}
		w.Header().Set("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
		w.Header().Set("Access-Control-Allow-Headers", "Content-Type")
		if r.Method == http.MethodOptions {
			w.WriteHeader(http.StatusNoContent)
			return
		}
		next.ServeHTTP(w, r)
	})
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}

// ========== GET /api/themes ==========

type themeListItem struct {
	Slug         string `json:"slug"`
	DisplayName  string `json:"display_name"`
	IsBuiltin    bool   `json:"is_builtin"`
	IsUserTheme  bool   `json:"is_user_theme"`
	HasLightDark bool   `json:"has_light_dark"`
}

func (ts *ThemeServer) handleListThemes(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}
	mgr := theme.NewManager(nil)
	infos := mgr.ListAvailableThemeInfos()
	list := make([]themeListItem, 0, len(infos))
	for _, info := range infos {
		isBuiltin := theme.BuiltinThemeIDs[info.ID]
		hasLightDark := false
		if err := mgr.LoadTheme(info.ID); err == nil {
			if t := mgr.GetCurrentTheme(); t != nil {
				hasLightDark = t.HasV3Schema()
			}
		}
		list = append(list, themeListItem{
			Slug:         info.ID,
			DisplayName:  info.DisplayName,
			IsBuiltin:    isBuiltin,
			IsUserTheme:  !isBuiltin,
			HasLightDark: hasLightDark,
		})
	}
	writeJSON(w, http.StatusOK, list)
}

// ========== GET /api/theme/:slug ==========

type themeYAMLResponse struct {
	Slug string `json:"slug"`
	YAML string `json:"yaml"`
}

func (ts *ThemeServer) handleGetTheme(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}
	slug := strings.TrimPrefix(r.URL.Path, "/api/theme/")
	if slug == "" || strings.Contains(slug, "/") || strings.Contains(slug, "..") {
		http.Error(w, "invalid slug", http.StatusBadRequest)
		return
	}
	data, err := findThemeYAML(slug)
	if err != nil {
		http.Error(w, err.Error(), http.StatusNotFound)
		return
	}
	writeJSON(w, http.StatusOK, themeYAMLResponse{Slug: slug, YAML: string(data)})
}

// ========== POST /api/theme/push ==========

type pushThemeRequest struct {
	YAML  string `json:"yaml"`
	Force bool   `json:"force"`
}

type pushThemeResponse struct {
	Imported  bool   `json:"imported"`
	Reloaded  bool   `json:"reloaded"`
	ThemeName string `json:"theme_name"`
}

func (ts *ThemeServer) handlePushTheme(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}
	if ts.onPush == nil {
		http.Error(w, "server not configured", http.StatusInternalServerError)
		return
	}
	var req pushThemeRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		http.Error(w, "invalid request body", http.StatusBadRequest)
		return
	}
	result := ts.onPush(req.YAML, req.Force)
	if result.Conflict {
		writeJSON(w, http.StatusConflict, map[string]any{
			"conflict":      true,
			"existing_name": result.ThemeName,
		})
		return
	}
	if !result.Success {
		writeJSON(w, http.StatusBadRequest, map[string]string{"error": result.ErrorMsg})
		return
	}
	reloaded := false
	if ts.onReload != nil {
		reloaded = ts.onReload()
	}
	writeJSON(w, http.StatusOK, pushThemeResponse{
		Imported:  true,
		Reloaded:  reloaded,
		ThemeName: result.ThemeName,
	})
}

// ========== App Wails 导出方法 ==========

// StartThemeServer 开启在线主题编辑服务（供 Wails 前端调用）。
func (a *App) StartThemeServer() (ThemeServerStatus, error) {
	if a.themeServer == nil {
		a.themeServer = &ThemeServer{
			onPush: func(y string, force bool) ImportThemeResult {
				return importThemeFromContent([]byte(y), force)
			},
			onReload: func() bool {
				if a.rpcClient == nil {
					return false
				}
				err := a.rpcClient.SystemNotifyReload("config")
				return err == nil
			},
		}
	}
	// 已在运行则直接返回当前状态
	if a.themeServer.server != nil {
		return a.themeServer.Status(), nil
	}
	if err := a.themeServer.Start(); err != nil {
		return ThemeServerStatus{}, err
	}
	return a.themeServer.Status(), nil
}

// StopThemeServer 停止在线主题编辑服务（供 Wails 前端调用）。
func (a *App) StopThemeServer() {
	if a.themeServer != nil {
		a.themeServer.Stop()
	}
}

// GetThemeServerStatus 获取在线主题编辑服务当前状态（供 Wails 前端调用）。
func (a *App) GetThemeServerStatus() ThemeServerStatus {
	if a.themeServer == nil {
		return ThemeServerStatus{}
	}
	return a.themeServer.Status()
}

// ========== 工具函数 ==========

// findThemeYAML 在用户主题目录和系统主题目录中查找指定 slug 的 theme.yaml 内容。
// 用户目录优先，以支持同名覆盖的用户主题。
func findThemeYAML(slug string) ([]byte, error) {
	if userDir, err := config.GetThemesUserDir(); err == nil {
		p := filepath.Join(userDir, slug, "theme.yaml")
		if data, err := os.ReadFile(p); err == nil {
			return data, nil
		}
	}
	if exeDir, err := config.GetExeDir(); err == nil {
		p := filepath.Join(config.GetDataDir(exeDir), "themes", slug, "theme.yaml")
		if data, err := os.ReadFile(p); err == nil {
			return data, nil
		}
	}
	return nil, fmt.Errorf("主题 %q 不存在", slug)
}
