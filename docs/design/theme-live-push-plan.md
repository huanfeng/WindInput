# Theme Live Push 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 `wind_setting` 内嵌轻量 HTTP 服务，让 Web 编辑器能一键拉取/推送主题并触发热重载。

**Architecture:** `ThemeServer` 结构体内嵌 `net/http.Server`，从端口 29731 起最多尝试 3 个端口；handler 层通过函数字段注入依赖，与 Wails 运行时解耦，可用 `httptest` 直接测试；App 在 Wails startup/shutdown 中管理服务器生命周期，通过 3 个 Wails 导出方法供前端控制。

**Tech Stack:** Go `net/http` / `httptest`、Wails v2、Vue 3 Composition API、shadcn/ui Switch + Button

---

## 文件索引

| 文件 | 操作 | 说明 |
|------|------|------|
| `wind_setting/theme_server.go` | 新建 | ThemeServer 结构体、端口绑定、CORS、3 个 HTTP 端点、App Wails 方法 |
| `wind_setting/theme_server_test.go` | 新建 | 端口自增、CORS preflight、3 个端点的 httptest 测试 |
| `wind_setting/app.go` | 修改 | App 结构体加 `themeServer` 字段；shutdown 加 Stop 调用 |
| `wind_setting/frontend/src/api/wails.ts` | 修改 | 新增 `ThemeServerStatus` 类型及 3 个绑定函数 |
| `wind_setting/frontend/src/pages/AppearancePage.vue` | 修改 | 新增"在线编辑"卡片（开关 + 状态 + 复制地址） |

---

## Task 1: ThemeServer 核心实现 + 测试

**Files:**
- Create: `wind_setting/theme_server.go`
- Create: `wind_setting/theme_server_test.go`

- [ ] **Step 1: 新建 `theme_server.go`，写入完整实现**

```go
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
	onReload func()
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
	for i := 0; i < themeServerMaxTries; i++ {
		port := themeServerBasePort + i
		ln, err := net.Listen("tcp", "127.0.0.1:"+strconv.Itoa(port))
		if err != nil {
			lastErr = err
			continue
		}
		ts.port = port
		ts.server = &http.Server{Handler: handler}
		go func() { _ = ts.server.Serve(ln) }()
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

// corsMiddleware 允许所有来源（本地服务，无鉴权需求）。
func corsMiddleware(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Access-Control-Allow-Origin", "*")
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
	Slug        string `json:"slug"`
	DisplayName string `json:"display_name"`
	IsBuiltin   bool   `json:"is_builtin"`
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
		list = append(list, themeListItem{
			Slug:        info.ID,
			DisplayName: info.DisplayName,
			IsBuiltin:   theme.BuiltinThemeIDs[info.ID],
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
		ts.onReload()
		reloaded = true
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
			onReload: func() {
				if a.rpcClient != nil {
					_ = a.rpcClient.SystemNotifyReload("theme")
				}
			},
		}
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
```

- [ ] **Step 2: 新建 `theme_server_test.go`**

```go
package main

import (
	"bytes"
	"encoding/json"
	"net"
	"net/http"
	"net/http/httptest"
	"testing"
)

// newTestServer 返回带 mock onPush 的测试服务实例。
// onReload 设为空函数（不实际触发 RPC）。
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
		onReload: func() {},
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
	w := httptest.NewRecorder()
	handler.ServeHTTP(w, req)

	if w.Code != http.StatusNoContent {
		t.Errorf("preflight 应返回 204，got %d", w.Code)
	}
	if got := w.Header().Get("Access-Control-Allow-Origin"); got != "*" {
		t.Errorf("CORS header 应为 *，got %q", got)
	}
}
```

- [ ] **Step 3: 运行测试，确认通过**

```
cd D:/Develop/workspace/go_dev/WindInput/wind_setting
go test -v -run TestThemeServer .
```

期望输出：所有 `TestThemeServer_*` 测试 PASS（`HandleListThemes` 依赖真实主题文件，无主题时返回空列表也算通过）

- [ ] **Step 4: 格式化**

```
cd D:/Develop/workspace/go_dev/WindInput/wind_setting
go fmt ./...
```

- [ ] **Step 5: 编译验证**

```
cd D:/Develop/workspace/go_dev/WindInput/wind_setting
go build ./...
```

期望：无报错

- [ ] **Step 6: Commit**

```
git add wind_setting/theme_server.go wind_setting/theme_server_test.go
git commit -m "feat(setting): ThemeServer HTTP 服务 + 端点测试（主题在线推送）"
```

---

## Task 2: 集成到 App 生命周期

**Files:**
- Modify: `wind_setting/app.go`

- [ ] **Step 1: 在 `app.go` 的 `App` 结构体中新增 `themeServer` 字段**

找到 `App` 结构体（约第 17 行），在 `startupUpdateResult` 字段后添加：

```go
// themeServer 在线主题编辑 HTTP 服务（开关由前端控制）
themeServer *ThemeServer
```

完整结构体变为：

```go
type App struct {
	ctx context.Context

	startPage     string
	addWordParams AddWordParams

	rpcClient *rpcapi.Client

	startupUpdateMu     sync.Mutex
	startupUpdateResult *updater.CheckResult

	themeServer *ThemeServer
}
```

- [ ] **Step 2: 在 `shutdown` 方法中加入停止逻辑**

找到 `shutdown` 方法（约第 89 行）：

```go
// 修改前：
func (a *App) shutdown(ctx context.Context) {}

// 修改后：
func (a *App) shutdown(ctx context.Context) {
	if a.themeServer != nil {
		a.themeServer.Stop()
	}
}
```

- [ ] **Step 3: 编译验证**

```
cd D:/Develop/workspace/go_dev/WindInput/wind_setting
go build ./...
```

- [ ] **Step 4: Commit**

```
git add wind_setting/app.go
git commit -m "feat(setting): App 集成 ThemeServer 生命周期管理"
```

---

## Task 3: 前端 Wails 类型绑定

**Files:**
- Modify: `wind_setting/frontend/src/api/wails.ts`

- [ ] **Step 1: 在 `wails.ts` 末尾新增 `ThemeServerStatus` 类型和 3 个绑定函数**

在文件末尾追加：

```typescript
// ========== 在线主题编辑服务 ==========

export interface ThemeServerStatus {
  running: boolean;
  port: number;
  url: string;
}

export function startThemeServer(): Promise<ThemeServerStatus> {
  return (window as any).go.main.App.StartThemeServer();
}

export function stopThemeServer(): Promise<void> {
  return (window as any).go.main.App.StopThemeServer();
}

export function getThemeServerStatus(): Promise<ThemeServerStatus> {
  return (window as any).go.main.App.GetThemeServerStatus();
}
```

- [ ] **Step 2: 前端构建验证**

```
cd D:/Develop/workspace/go_dev/WindInput/wind_setting/frontend
pnpm build
```

期望：无 TypeScript 错误

- [ ] **Step 3: Commit**

```
git add wind_setting/frontend/src/api/wails.ts
git commit -m "feat(setting): wails.ts 新增 ThemeServer 状态类型与绑定"
```

---

## Task 4: 前端"在线编辑"卡片 UI

**Files:**
- Modify: `wind_setting/frontend/src/pages/AppearancePage.vue`

- [ ] **Step 1: 在 `<script setup>` 中新增导入和响应式状态**

在现有导入块中（`import { ref, computed, ... }` 所在行），新增对 wails.ts 函数的引用：

```typescript
import {
  startThemeServer,
  stopThemeServer,
  getThemeServerStatus,
} from "../api/wails";
import type { ThemeServerStatus } from "../api/wails";
```

在现有响应式变量（如 `const themeImportOpen = ref(false)` 附近）后追加：

```typescript
const themeServerRunning = ref(false);
const themeServerURL = ref("");
const themeServerError = ref("");
```

- [ ] **Step 2: 在 `onMounted` 钩子中初始化服务器状态**

找到现有的 `onMounted` 函数，在其内部末尾追加（`isWailsEnv` 已作为 prop 存在）：

```typescript
if (props.isWailsEnv) {
  const status = await getThemeServerStatus();
  themeServerRunning.value = status.running;
  themeServerURL.value = status.url;
}
```

- [ ] **Step 3: 新增 `toggleThemeServer` 和 `copyServerURL` 函数**

在 `onThemeImported` 函数后追加：

```typescript
async function toggleThemeServer(enabled: boolean) {
  themeServerError.value = "";
  if (enabled) {
    try {
      const status = await startThemeServer();
      themeServerRunning.value = true;
      themeServerURL.value = status.url;
    } catch (e) {
      themeServerError.value = String(e);
    }
  } else {
    await stopThemeServer();
    themeServerRunning.value = false;
    themeServerURL.value = "";
  }
}

async function copyServerURL() {
  if (themeServerURL.value) {
    await navigator.clipboard.writeText(themeServerURL.value);
  }
}
```

- [ ] **Step 4: 在模板中新增"在线编辑"卡片**

在外观页模板的最后一个卡片 `</div>` 之后、最外层 `</div>` 之前，追加以下卡片（参照页面中现有的卡片结构，如主题导入卡片的样式）：

```vue
<!-- 在线编辑 -->
<div v-if="isWailsEnv" class="rounded-lg border bg-card p-4 space-y-3">
  <div class="font-medium text-sm">在线编辑</div>
  <div class="flex items-center justify-between">
    <div class="space-y-0.5">
      <p class="text-sm">开启在线连接</p>
      <p class="text-xs text-muted-foreground">
        允许 Web 编辑器推送主题到本地输入法
      </p>
    </div>
    <Switch
      :checked="themeServerRunning"
      @update:checked="toggleThemeServer"
    />
  </div>
  <div
    v-if="themeServerRunning"
    class="flex items-center gap-2 text-sm"
  >
    <span class="text-green-500 text-xs">●</span>
    <span class="text-muted-foreground flex-1 truncate">{{ themeServerURL }}</span>
    <Button size="sm" variant="outline" @click="copyServerURL">
      复制地址
    </Button>
  </div>
  <p v-if="themeServerError" class="text-xs text-destructive">
    {{ themeServerError }}
  </p>
</div>
```

- [ ] **Step 5: 前端构建验证**

```
cd D:/Develop/workspace/go_dev/WindInput/wind_setting/frontend
pnpm build
```

期望：无 TypeScript/Vue 报错

- [ ] **Step 6: 完整应用编译验证**

```
cd D:/Develop/workspace/go_dev/WindInput/wind_setting
go build ./...
```

- [ ] **Step 7: Commit**

```
git add wind_setting/frontend/src/pages/AppearancePage.vue
git commit -m "feat(setting): 外观页新增在线编辑卡片（ThemeServer 开关）"
```

---

## 完成检查清单

- [ ] `go test -v -run TestThemeServer ./wind_setting/...` 全部 PASS
- [ ] `wind_setting/go build ./...` 无报错
- [ ] `frontend/pnpm build` 无报错
- [ ] 设置页外观 tab 底部出现"在线编辑"卡片
- [ ] 开关开启后显示 `http://localhost:29731`（或自动递增后的端口）
- [ ] 从浏览器访问 `http://localhost:29731/api/themes` 返回 JSON 数组
- [ ] 关闭设置页后端口释放（再次打开时可正常开启）
