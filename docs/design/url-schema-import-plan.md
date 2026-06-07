# URL Schema 一键导入（windinput://）实现计划

> **For agentic workers:** 配套设计见 `docs/design/url-schema-import.md`。按任务顺序逐项实现，步骤用 `- [ ]` 勾选。

**Goal:** 为清风输入法实现自定义 URL 协议 `windinput://`，支持从浏览器一键唤起设置程序并弹确认框导入主题（方案/词库/扩展词库预留扩展点），跨 Windows / macOS。

**Architecture:** 设置程序 `wind_setting` 独占协议注册与导入编排，输入法核心服务不参与。Windows 运行时写 HKCU + 安装器写 HKCU + os.Args/IPC 接收；macOS 声明式 Info.plist + Wails `mac.OnUrlOpen` 接收。导入逻辑/解析/确认框三块跨平台共享，注册与接收入口按 `_windows.go`/`_darwin.go` 分文件。

**Tech Stack:** Go 1.x + Wails v2.12.0 + Vue 3 + `golang.org/x/sys/windows/registry`（v0.43.0 已含）+ NSIS。

---

## 文件结构

```
新建（共享，平台无关）:
  wind_setting/protocol_url.go            URL 解析 → ProtocolRequest
  wind_setting/protocol_url_test.go       解析单测
  wind_setting/protocol_handler.go        handleProtocolURL / ConsumePendingProtocol / GetProtocolStatus / SetProtocolRegistered
  wind_setting/protocol_handler_test.go   payload 构造单测
新建（平台分文件）:
  wind_setting/protocol_register_windows.go       HKCU 写/删/对账/自愈 + managed=false
  wind_setting/protocol_register_windows_test.go  注册对账单测（隔离测试键）
  wind_setting/protocol_register_darwin.go        stub no-op + managed=true
新建（前端）:
  wind_setting/frontend/src/components/ProtocolImportDialog.vue  通用导入确认框（本期 theme 分支）
改:
  wind_setting/app.go                     App 结构加 pendingProtocol/pendingMu/protocolURL；startup 接线
  wind_setting/app_theme.go               新增 PreviewThemeFromURL（下载+解析 meta，不落盘）
  wind_setting/main.go                    parseProtocolArg + Mac.OnUrlOpen + 传 protocolURL + 单例透传
  wind_setting/singleton_windows.go       ensureSingleInstance 加 protocolURL 参数；IPC 支持 protocol|；startIPCListener 加 *App
  wind_setting/singleton_darwin.go        同步签名（no-op）
  wind_setting/wails.json                 info.protocols
  wind_setting/frontend/src/api/wails.ts  前端 Wails 包装函数
  wind_setting/frontend/src/App.vue       EventsOn("protocol-import") + onMounted 拉 pending
  wind_setting/frontend/src/pages/AdvancedPage.vue  注册开关区块
  installer/nsis/WindInput.nsi            安装写 HKCU 协议键 / 卸载删键
```

---

## Task 1: URL 解析器（纯 Go，跨平台）

**Files:**
- Create: `wind_setting/protocol_url.go`
- Test: `wind_setting/protocol_url_test.go`

- [ ] **Step 1: 写失败测试**

`wind_setting/protocol_url_test.go`:
```go
package main

import "testing"

func TestParseProtocolURL(t *testing.T) {
	t.Run("合法主题链接", func(t *testing.T) {
		req, err := ParseProtocolURL("windinput://import/theme?url=https%3A%2F%2Fsite.com%2Fa.yaml&name=Dark")
		if err != nil {
			t.Fatalf("unexpected err: %v", err)
		}
		if req.Kind != "theme" {
			t.Errorf("kind = %q, want theme", req.Kind)
		}
		if req.URL != "https://site.com/a.yaml" {
			t.Errorf("url = %q", req.URL)
		}
		if req.Name != "Dark" {
			t.Errorf("name = %q", req.Name)
		}
	})
	t.Run("scheme 错误", func(t *testing.T) {
		if _, err := ParseProtocolURL("http://import/theme?url=https://x"); err == nil {
			t.Error("want error for wrong scheme")
		}
	})
	t.Run("未知 kind", func(t *testing.T) {
		if _, err := ParseProtocolURL("windinput://import/unknown?url=https://x"); err == nil {
			t.Error("want error for unknown kind")
		}
	})
	t.Run("缺 url 参数", func(t *testing.T) {
		if _, err := ParseProtocolURL("windinput://import/theme"); err == nil {
			t.Error("want error for missing url")
		}
	})
	t.Run("非 https 拒绝", func(t *testing.T) {
		if _, err := ParseProtocolURL("windinput://import/theme?url=http://x/a.yaml"); err == nil {
			t.Error("want error for non-https url")
		}
	})
	t.Run("预留 kind 可解析", func(t *testing.T) {
		for _, k := range []string{"schema", "dict", "extdict"} {
			if _, err := ParseProtocolURL("windinput://import/" + k + "?url=https://x/a"); err != nil {
				t.Errorf("kind %s should parse: %v", k, err)
			}
		}
	})
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd wind_setting && go test ./... -run TestParseProtocolURL -v`
Expected: FAIL（`ParseProtocolURL` 未定义，编译错误）

- [ ] **Step 3: 实现 protocol_url.go**

`wind_setting/protocol_url.go`:
```go
package main

import (
	"fmt"
	"net/url"
	"strings"
)

// ProtocolRequest 是 windinput:// 协议解析后的结构化请求。
type ProtocolRequest struct {
	Kind string `json:"kind"` // theme | schema | dict | extdict
	URL  string `json:"url"`  // 待下载的 https 直链
	Name string `json:"name,omitempty"` // 可选显示名（不可信，仅作提示）
}

// validProtocolKinds 支持的导入类型集合（本期仅 theme 实现 UI，其余预留）。
var validProtocolKinds = map[string]bool{
	"theme": true, "schema": true, "dict": true, "extdict": true,
}

// ParseProtocolURL 解析 windinput://import/<kind>?url=...&name=...。
// 入口安全收紧：url 参数必须为 https。
func ParseProtocolURL(raw string) (*ProtocolRequest, error) {
	u, err := url.Parse(strings.TrimSpace(raw))
	if err != nil {
		return nil, fmt.Errorf("无法解析链接: %w", err)
	}
	if !strings.EqualFold(u.Scheme, "windinput") {
		return nil, fmt.Errorf("不支持的协议: %s", u.Scheme)
	}
	if action := strings.ToLower(u.Host); action != "import" {
		return nil, fmt.Errorf("不支持的操作: %s", action)
	}
	kind := strings.ToLower(strings.Trim(u.Path, "/"))
	if !validProtocolKinds[kind] {
		return nil, fmt.Errorf("不支持的导入类型: %s", kind)
	}
	q := u.Query()
	target := strings.TrimSpace(q.Get("url"))
	if target == "" {
		return nil, fmt.Errorf("缺少 url 参数")
	}
	if !strings.HasPrefix(strings.ToLower(target), "https://") {
		return nil, fmt.Errorf("url 参数必须是 https 链接")
	}
	return &ProtocolRequest{Kind: kind, URL: target, Name: strings.TrimSpace(q.Get("name"))}, nil
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cd wind_setting && go fmt ./... && go test ./... -run TestParseProtocolURL -v`
Expected: PASS（全部子测试）

---

## Task 2: Windows 协议注册 + darwin stub

**Files:**
- Create: `wind_setting/protocol_register_windows.go`
- Create: `wind_setting/protocol_register_windows_test.go`
- Create: `wind_setting/protocol_register_darwin.go`

- [ ] **Step 1: 写失败测试（用隔离测试键，不污染真实注册表）**

`wind_setting/protocol_register_windows_test.go`:
```go
//go:build windows

package main

import (
	"testing"

	"golang.org/x/sys/windows/registry"
)

func TestRegisterProtocolAt(t *testing.T) {
	const testKey = `Software\Classes\windinput_unittest`
	exe := `C:\Test\wind_setting.exe`
	t.Cleanup(func() { _ = unregisterProtocolAt(registry.CURRENT_USER, testKey) })

	if err := registerProtocolAt(registry.CURRENT_USER, testKey, exe); err != nil {
		t.Fatalf("register: %v", err)
	}
	ok, cmd := protocolStatusAt(registry.CURRENT_USER, testKey)
	if !ok {
		t.Fatal("status should be registered")
	}
	want := `"` + exe + `" "%1"`
	if cmd != want {
		t.Errorf("cmd = %q, want %q", cmd, want)
	}
	if err := unregisterProtocolAt(registry.CURRENT_USER, testKey); err != nil {
		t.Fatalf("unregister: %v", err)
	}
	if ok, _ := protocolStatusAt(registry.CURRENT_USER, testKey); ok {
		t.Error("should be unregistered after unregister")
	}
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd wind_setting && go test ./... -run TestRegisterProtocolAt -v`
Expected: FAIL（`registerProtocolAt` 未定义）

- [ ] **Step 3: 实现 protocol_register_windows.go**

`wind_setting/protocol_register_windows.go`:
```go
//go:build windows

package main

import (
	"fmt"
	"os"
	"strings"

	"golang.org/x/sys/windows/registry"
)

// protocolManagedBySystem 表示协议注册是否由系统声明式托管（Windows=false，可运行时开关）。
const protocolManagedBySystem = false

const protocolScheme = "windinput"

// protocolKeyPath 是协议根键在 HKCU 下的路径（var 便于测试覆盖）。
var protocolKeyPath = `Software\Classes\` + protocolScheme

// protocolCommand 返回期望的 shell\open\command 值。
func protocolCommand(exePath string) string {
	return `"` + exePath + `" "%1"`
}

// RegisterProtocol 把 windinput:// 注册到当前用户 HKCU，command 指向当前可执行文件。
func RegisterProtocol() error {
	exe, err := os.Executable()
	if err != nil {
		return err
	}
	return registerProtocolAt(registry.CURRENT_USER, protocolKeyPath, exe)
}

func registerProtocolAt(root registry.Key, keyPath, exePath string) error {
	k, _, err := registry.CreateKey(root, keyPath, registry.WRITE)
	if err != nil {
		return fmt.Errorf("创建协议键失败: %w", err)
	}
	defer k.Close()
	if err := k.SetStringValue("", "URL:清风输入法协议"); err != nil {
		return err
	}
	if err := k.SetStringValue("URL Protocol", ""); err != nil {
		return err
	}
	cmdKey, _, err := registry.CreateKey(root, keyPath+`\shell\open\command`, registry.WRITE)
	if err != nil {
		return fmt.Errorf("创建 command 键失败: %w", err)
	}
	defer cmdKey.Close()
	return cmdKey.SetStringValue("", protocolCommand(exePath))
}

// UnregisterProtocol 删除当前用户的协议键。
func UnregisterProtocol() error {
	return unregisterProtocolAt(registry.CURRENT_USER, protocolKeyPath)
}

func unregisterProtocolAt(root registry.Key, keyPath string) error {
	// DeleteKey 要求目标无子键，自底向上逐层删除。
	_ = registry.DeleteKey(root, keyPath+`\shell\open\command`)
	_ = registry.DeleteKey(root, keyPath+`\shell\open`)
	_ = registry.DeleteKey(root, keyPath+`\shell`)
	return registry.DeleteKey(root, keyPath)
}

// ProtocolStatus 返回 (是否已注册, 当前 command)。
func ProtocolStatus() (bool, string) {
	return protocolStatusAt(registry.CURRENT_USER, protocolKeyPath)
}

func protocolStatusAt(root registry.Key, keyPath string) (bool, string) {
	k, err := registry.OpenKey(root, keyPath+`\shell\open\command`, registry.QUERY_VALUE)
	if err != nil {
		return false, ""
	}
	defer k.Close()
	cmd, _, err := k.GetStringValue("")
	if err != nil {
		return false, ""
	}
	return true, cmd
}

// SelfHealProtocol 在设置程序启动时对账：缺失或 command 与当前 exe 不符则重写。
// 覆盖便携版移动、版本升级换路径的场景。
func SelfHealProtocol() {
	exe, err := os.Executable()
	if err != nil {
		return
	}
	registered, cmd := ProtocolStatus()
	if !registered || !strings.EqualFold(cmd, protocolCommand(exe)) {
		_ = RegisterProtocol()
	}
}
```

- [ ] **Step 4: 实现 darwin stub**

`wind_setting/protocol_register_darwin.go`:
```go
//go:build darwin

package main

// macOS 上 windinput:// 由 Info.plist 的 CFBundleURLTypes 声明式注册，
// 随 .app 打包、LaunchServices 自动登记，无运行时写入需求。

// protocolManagedBySystem=true：前端据此把注册区块显示为只读。
const protocolManagedBySystem = true

func RegisterProtocol() error   { return nil }
func UnregisterProtocol() error { return nil }
func ProtocolStatus() (bool, string) {
	return true, "由系统管理（随应用注册）"
}
func SelfHealProtocol() {}
```

- [ ] **Step 5: 跑测试 + 编译确认通过**

Run: `cd wind_setting && go fmt ./... && go test ./... -run TestRegisterProtocolAt -v && go build ./...`
Expected: PASS + 编译成功

---

## Task 3: 协议处理与前端投递（共享）

**Files:**
- Modify: `wind_setting/app.go`（App 结构 + import sync）
- Create: `wind_setting/protocol_handler.go`
- Create: `wind_setting/protocol_handler_test.go`

- [ ] **Step 1: 给 App 结构加字段**

`wind_setting/app.go`，在 `type App struct {` 块内（`themeServer` 字段后）追加：
```go
	// pendingProtocol 缓存冷启动/早于前端就绪时收到的协议导入请求，
	// 由前端 onMounted 调 ConsumePendingProtocol 拉取（消除 emit 早于 EventsOn 的竞争）。
	pendingProtocol *ProtocolImportPayload
	pendingMu       sync.Mutex

	// protocolURL 是 Windows 冷启动时从 os.Args 解析出的协议链接，在 startup 中处理。
	protocolURL string
```
并确保 `app.go` 顶部 import 含 `"sync"`（若无则添加）。

- [ ] **Step 2: 写失败测试**

`wind_setting/protocol_handler_test.go`:
```go
package main

import "testing"

func TestBuildProtocolPayload(t *testing.T) {
	t.Run("合法", func(t *testing.T) {
		p := buildProtocolPayload("windinput://import/theme?url=https%3A%2F%2Fx.com%2Fa.yaml")
		if !p.OK {
			t.Fatalf("want ok, got error: %s", p.Error)
		}
		if p.Request == nil || p.Request.Kind != "theme" {
			t.Errorf("bad request: %+v", p.Request)
		}
	})
	t.Run("非法", func(t *testing.T) {
		p := buildProtocolPayload("windinput://import/theme")
		if p.OK {
			t.Error("want not ok")
		}
		if p.Error == "" {
			t.Error("want error message")
		}
	})
}

func TestConsumePendingProtocol(t *testing.T) {
	a := &App{}
	a.handleProtocolURL("windinput://import/theme?url=https://x.com/a.yaml")
	p := a.ConsumePendingProtocol()
	if p == nil || !p.OK {
		t.Fatal("want cached payload")
	}
	if a.ConsumePendingProtocol() != nil {
		t.Error("pending should be cleared after consume")
	}
}
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cd wind_setting && go test ./... -run "TestBuildProtocolPayload|TestConsumePendingProtocol" -v`
Expected: FAIL（未定义符号）

- [ ] **Step 4: 实现 protocol_handler.go**

`wind_setting/protocol_handler.go`:
```go
package main

import (
	wailsRuntime "github.com/wailsapp/wails/v2/pkg/runtime"
)

// ProtocolImportPayload 是投递给前端的协议导入负载（含解析成功/失败）。
type ProtocolImportPayload struct {
	OK      bool             `json:"ok"`
	Error   string           `json:"error,omitempty"`
	Request *ProtocolRequest `json:"request,omitempty"`
}

// ProtocolRegStatus 协议注册状态（供设置页展示）。
type ProtocolRegStatus struct {
	Registered bool   `json:"registered"`
	Command    string `json:"command"`
	Managed    bool   `json:"managed"` // true=系统托管(macOS)，前端只读
}

func buildProtocolPayload(raw string) *ProtocolImportPayload {
	req, err := ParseProtocolURL(raw)
	if err != nil {
		return &ProtocolImportPayload{OK: false, Error: err.Error()}
	}
	return &ProtocolImportPayload{OK: true, Request: req}
}

// handleProtocolURL 解析协议链接，缓存为 pending 并（若前端就绪）emit 事件。
func (a *App) handleProtocolURL(raw string) {
	payload := buildProtocolPayload(raw)
	a.pendingMu.Lock()
	a.pendingProtocol = payload
	a.pendingMu.Unlock()
	if a.ctx != nil {
		wailsRuntime.EventsEmit(a.ctx, "protocol-import", payload)
	}
}

// ConsumePendingProtocol 前端 onMounted 主动拉取并清空缓存（Wails 导出）。
func (a *App) ConsumePendingProtocol() *ProtocolImportPayload {
	a.pendingMu.Lock()
	defer a.pendingMu.Unlock()
	p := a.pendingProtocol
	a.pendingProtocol = nil
	return p
}

// GetProtocolStatus 返回协议注册状态（Wails 导出）。
func (a *App) GetProtocolStatus() ProtocolRegStatus {
	reg, cmd := ProtocolStatus()
	return ProtocolRegStatus{Registered: reg, Command: cmd, Managed: protocolManagedBySystem}
}

// SetProtocolRegistered 注册/注销协议（Wails 导出，macOS 上为 no-op）。
func (a *App) SetProtocolRegistered(enabled bool) error {
	if enabled {
		return RegisterProtocol()
	}
	return UnregisterProtocol()
}
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cd wind_setting && go fmt ./... && go test ./... -run "TestBuildProtocolPayload|TestConsumePendingProtocol" -v && go build ./...`
Expected: PASS + 编译成功

---

## Task 4: 主题 URL 预览（确认框数据源）

**Files:**
- Modify: `wind_setting/app_theme.go`

- [ ] **Step 1: 写失败测试**

`wind_setting/app_theme_test.go` 末尾追加：
```go
func TestParseThemePreviewMeta(t *testing.T) {
	yaml := "meta:\n  name: 暗夜\n  author: 张三\n  version: \"1.2\"\n"
	name, author, version := parseThemePreviewMeta([]byte(yaml))
	if name != "暗夜" || author != "张三" || version != "1.2" {
		t.Errorf("got %q/%q/%q", name, author, version)
	}
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd wind_setting && go test ./... -run TestParseThemePreviewMeta -v`
Expected: FAIL（`parseThemePreviewMeta` 未定义）

- [ ] **Step 3: 在 app_theme.go 增加预览方法**

`wind_setting/app_theme.go` 追加（复用文件已有 `http`/`io`/`time`/`yaml` import）：
```go
// ThemeURLPreview 主题 URL 预览结果（确认框展示用）。
// YAML 字段回传原始内容，确认导入时走 ImportThemeFromText，避免二次下载。
type ThemeURLPreview struct {
	OK          bool   `json:"ok"`
	Name        string `json:"name"`
	Author      string `json:"author"`
	Version     string `json:"version"`
	Description string `json:"description"`
	SourceURL   string `json:"source_url"`
	YAML        string `json:"yaml"`
	ErrorMsg    string `json:"error_msg"`
}

// parseThemePreviewMeta 从 YAML 内容提取 meta 字段（不校验完整性）。
func parseThemePreviewMeta(content []byte) (name, author, version string) {
	var t theme.Theme
	if err := yaml.Unmarshal(content, &t); err != nil {
		return "", "", ""
	}
	return t.Meta.Name, t.Meta.Author, t.Meta.Version
}

// PreviewThemeFromURL 下载并解析主题 meta（不落盘），供 URL schema 确认框展示。
func (a *App) PreviewThemeFromURL(rawURL string) ThemeURLPreview {
	rawURL = strings.TrimSpace(rawURL)
	if !strings.HasPrefix(rawURL, "https://") {
		return ThemeURLPreview{ErrorMsg: "仅支持 https 链接"}
	}
	client := &http.Client{Timeout: 15 * time.Second}
	resp, err := client.Get(rawURL) //nolint:noctx
	if err != nil {
		return ThemeURLPreview{ErrorMsg: "下载失败: " + err.Error()}
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return ThemeURLPreview{ErrorMsg: fmt.Sprintf("下载失败，服务器返回 %d", resp.StatusCode)}
	}
	const maxSize = 1 << 20
	content, err := io.ReadAll(io.LimitReader(resp.Body, maxSize))
	if err != nil {
		return ThemeURLPreview{ErrorMsg: "读取内容失败: " + err.Error()}
	}
	name, author, version := parseThemePreviewMeta(content)
	if name == "" {
		return ThemeURLPreview{ErrorMsg: "主题缺少 meta.name 或格式错误"}
	}
	var t theme.Theme
	_ = yaml.Unmarshal(content, &t)
	return ThemeURLPreview{
		OK: true, Name: name, Author: author, Version: version,
		Description: t.Meta.Description, SourceURL: rawURL, YAML: string(content),
	}
}
```

- [ ] **Step 4: 跑测试 + 编译确认通过**

Run: `cd wind_setting && go fmt ./... && go test ./... -run TestParseThemePreviewMeta -v && go build ./...`
Expected: PASS + 编译成功

---

## Task 5: 入口接线 —— main.go / 单实例 / startup

**Files:**
- Modify: `wind_setting/main.go`
- Modify: `wind_setting/singleton_windows.go`
- Modify: `wind_setting/singleton_darwin.go`
- Modify: `wind_setting/app.go`（startup）

- [ ] **Step 1: main.go 解析协议参数并传递**

`wind_setting/main.go` 增加解析函数（放在 `parseStartPage` 后）：
```go
// parseProtocolArg 从命令行参数中找出 windinput:// 协议链接（Windows 主路径）。
func parseProtocolArg() string {
	for _, arg := range os.Args[1:] {
		if strings.HasPrefix(strings.ToLower(arg), "windinput://") {
			return arg
		}
	}
	return ""
}
```
在 `main()` 中，`startPage := parseStartPage()` 后加：
```go
	protocolURL := parseProtocolArg()
```
把单例检查改为传入 protocolURL：
```go
	releaseInstance, ok := ensureSingleInstance(startPage, addWordParams, protocolURL)
```
在 `app.startPage = startPage` 附近加：
```go
	app.protocolURL = protocolURL
```

- [ ] **Step 2: main.go 注入 mac.OnUrlOpen**

`wind_setting/main.go` 顶部 import 增加：
```go
	wailsMac "github.com/wailsapp/wails/v2/pkg/options/mac"
```
在 `wails.Run(&options.App{...})` 的选项中增加 `Mac` 字段（与 `Windows:` 同级）：
```go
		Mac: &wailsMac.Options{
			OnUrlOpen: func(url string) {
				app.handleProtocolURL(url)
			},
		},
```

- [ ] **Step 3: singleton_windows.go 支持 protocol 透传**

`wind_setting/singleton_windows.go`：
1. `ensureSingleInstance` 签名改为：
```go
func ensureSingleInstance(startPage string, addWordParams AddWordParams, protocolURL string) (func(), bool) {
```
2. 在已有实例分支（`if startPage != "" {...}` 之后）追加：
```go
		if protocolURL != "" {
			sendPageToExisting("protocol|" + protocolURL)
		}
```
3. `startIPCListener` 签名加 `*App` 参数：
```go
func startIPCListener(ctx context.Context, app *App) {
```
4. 在 IPC 监听循环解析消息处（`if strings.HasPrefix(raw, "add-word|")` 分支前）增加：
```go
				if strings.HasPrefix(raw, "protocol|") {
					url := strings.TrimPrefix(raw, "protocol|")
					app.handleProtocolURL(url)
					log.Printf("[singleton] 已处理协议导入请求")
					continue
				}
```

- [ ] **Step 4: singleton_darwin.go 同步签名**

`wind_setting/singleton_darwin.go`：
```go
func ensureSingleInstance(startPage string, addWordParams AddWordParams, protocolURL string) (func(), bool) {
	return func() {}, true
}
func startIPCListener(ctx context.Context, app *App) {}
```

- [ ] **Step 5: app.go startup 接线**

`wind_setting/app.go` `startup` 函数：
1. `startIPCListener(ctx)` 改为 `startIPCListener(ctx, a)`。
2. 在 `a.ctx = ctx` 后追加：
```go
	// 自愈注册协议（Windows 写 HKCU；macOS 为 no-op）
	SelfHealProtocol()
	// 处理 Windows 冷启动经 os.Args 传入的协议链接（macOS 走 mac.OnUrlOpen）
	if a.protocolURL != "" {
		a.handleProtocolURL(a.protocolURL)
	}
```

- [ ] **Step 6: 编译确认通过（Windows）**

Run: `cd wind_setting && go fmt ./... && go build ./...`
Expected: 编译成功

- [ ] **Step 7: 交叉编译确认 darwin 也通过**

Run: `cd wind_setting && $env:GOOS="darwin"; $env:GOARCH="arm64"; go build ./...; $env:GOOS=""; $env:GOARCH=""`
Expected: 编译成功（验证平台分文件签名一致）

---

## Task 6: wails.json 声明协议（macOS 注册）

**Files:**
- Modify: `wind_setting/wails.json`

- [ ] **Step 1: 在 info 块增加 protocols**

`wind_setting/wails.json` 的 `"info": { ... }` 内追加（与 `productName` 同级）：
```json
    "protocols": [
      {
        "scheme": "windinput",
        "description": "清风输入法协议",
        "role": "Viewer"
      }
    ]
```

- [ ] **Step 2: 验证 JSON 合法**

Run: `cd wind_setting && node -e "JSON.parse(require('fs').readFileSync('wails.json','utf8')); console.log('ok')"`
Expected: 输出 `ok`

> 说明：macOS 打包时 `scripts_mac/build/setting.sh` 走 `wails build`，Info.plist 模板的 `{{if .Info.Protocols}}` 块会据此生成 `CFBundleURLTypes`，LaunchServices 自动登记。Windows 不读此字段。

---

## Task 7: 前端 —— Wails 包装 + 确认框 + App.vue 接线

**Files:**
- Modify: `wind_setting/frontend/src/api/wails.ts`
- Create: `wind_setting/frontend/src/components/ProtocolImportDialog.vue`
- Modify: `wind_setting/frontend/src/App.vue`

- [ ] **Step 1: api/wails.ts 增加包装函数**

`wind_setting/frontend/src/api/wails.ts` 末尾追加（沿用文件内 `window.go.main.App` 动态调用风格，参照 `ImportThemeFromURL` 处）：
```ts
// ===== URL Schema 协议导入 =====
export interface ProtocolRequest {
  kind: string;
  url: string;
  name?: string;
}
export interface ProtocolImportPayload {
  ok: boolean;
  error?: string;
  request?: ProtocolRequest;
}
export interface ProtocolRegStatus {
  registered: boolean;
  command: string;
  managed: boolean;
}
export interface ThemeURLPreview {
  ok: boolean;
  name: string;
  author: string;
  version: string;
  description: string;
  source_url: string;
  yaml: string;
  error_msg: string;
}

export function consumePendingProtocol(): Promise<ProtocolImportPayload | null> {
  return (window as any).go.main.App.ConsumePendingProtocol();
}
export function getProtocolStatus(): Promise<ProtocolRegStatus> {
  return (window as any).go.main.App.GetProtocolStatus();
}
export function setProtocolRegistered(enabled: boolean): Promise<void> {
  return (window as any).go.main.App.SetProtocolRegistered(enabled);
}
export function previewThemeFromURL(url: string): Promise<ThemeURLPreview> {
  return (window as any).go.main.App.PreviewThemeFromURL(url);
}
```

- [ ] **Step 2: 创建确认框组件**

`wind_setting/frontend/src/components/ProtocolImportDialog.vue`:
```vue
<script setup lang="ts">
import { ref, watch } from "vue";
import {
  previewThemeFromURL,
  type ProtocolImportPayload,
  type ThemeURLPreview,
} from "../api/wails";
import { toast } from "vue-sonner";

const props = defineProps<{ payload: ProtocolImportPayload | null }>();
const emit = defineEmits<{ (e: "close"): void }>();

const open = ref(false);
const loading = ref(false);
const errorMsg = ref("");
const preview = ref<ThemeURLPreview | null>(null);
const kind = ref("");
const sourceHost = ref("");

function hostOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

watch(
  () => props.payload,
  async (p) => {
    if (!p) return;
    open.value = true;
    errorMsg.value = "";
    preview.value = null;
    if (!p.ok || !p.request) {
      errorMsg.value = p.error || "无效的导入链接";
      return;
    }
    kind.value = p.request.kind;
    sourceHost.value = hostOf(p.request.url);
    if (p.request.kind !== "theme") {
      errorMsg.value = `「${p.request.kind}」类型导入暂未支持`;
      return;
    }
    loading.value = true;
    try {
      const r = await previewThemeFromURL(p.request.url);
      if (!r.ok) {
        errorMsg.value = r.error_msg || "预览失败";
      } else {
        preview.value = r;
      }
    } catch (e: any) {
      errorMsg.value = String(e);
    } finally {
      loading.value = false;
    }
  },
  { immediate: true },
);

async function confirmImport(force = false) {
  if (!preview.value) return;
  loading.value = true;
  try {
    const res = await (window as any).go.main.App.ImportThemeFromText(
      preview.value.yaml,
      force,
    );
    if (res.conflict) {
      if (window.confirm(`已存在主题「${res.theme_name}」，是否覆盖？`)) {
        await confirmImport(true);
        return;
      }
    } else if (res.success) {
      toast.success(`主题「${res.theme_name}」已导入`);
      try {
        await (window as any).go.main.App.StartThemeServer();
      } catch {}
      close();
    } else {
      toast.error(res.error_msg || "导入失败");
    }
  } finally {
    loading.value = false;
  }
}

function close() {
  open.value = false;
  emit("close");
}
</script>

<template>
  <div v-if="open" class="protocol-dialog-mask" @click.self="close">
    <div class="protocol-dialog">
      <h3>导入主题</h3>
      <p v-if="loading">正在加载…</p>
      <p v-else-if="errorMsg" class="error">{{ errorMsg }}</p>
      <div v-else-if="preview" class="info">
        <div><b>名称：</b>{{ preview.name }}</div>
        <div v-if="preview.author"><b>作者：</b>{{ preview.author }}</div>
        <div v-if="preview.version"><b>版本：</b>{{ preview.version }}</div>
        <div v-if="preview.description">
          <b>说明：</b>{{ preview.description }}
        </div>
        <div class="source"><b>来源：</b>{{ sourceHost }}</div>
      </div>
      <div class="actions">
        <button @click="close">取消</button>
        <button
          v-if="preview && !errorMsg"
          :disabled="loading"
          class="primary"
          @click="confirmImport(false)"
        >
          导入
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.protocol-dialog-mask {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.35);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 9999;
}
.protocol-dialog {
  background: var(--background, #fff);
  color: var(--foreground, #111);
  border-radius: 10px;
  padding: 20px 24px;
  min-width: 320px;
  max-width: 480px;
  box-shadow: 0 10px 40px rgba(0, 0, 0, 0.25);
}
.protocol-dialog h3 {
  margin: 0 0 12px;
}
.info > div {
  margin: 4px 0;
  word-break: break-all;
}
.source {
  color: #888;
  font-size: 0.9em;
}
.error {
  color: #c0392b;
}
.actions {
  margin-top: 18px;
  display: flex;
  justify-content: flex-end;
  gap: 10px;
}
.actions button {
  padding: 6px 16px;
  border-radius: 6px;
  cursor: pointer;
}
.actions .primary {
  background: var(--primary, #3b82f6);
  color: #fff;
  border: none;
}
</style>
```

> 注：toast 用法须与项目一致。若项目用 `vue-sonner` 的具名 `toast`（参照 App.vue 顶部 `Sonner` 用法），保持上面写法；若不同，对齐 `App.vue` 现有 toast 调用方式。

- [ ] **Step 3: App.vue 接线**

`wind_setting/frontend/src/App.vue`：
1. `<script setup>` 顶部 import：
```ts
import ProtocolImportDialog from "./components/ProtocolImportDialog.vue";
import { consumePendingProtocol, type ProtocolImportPayload } from "./api/wails";
```
2. 增加响应式状态（与 `showAddWordDialog` 等同级）：
```ts
const protocolPayload = ref<ProtocolImportPayload | null>(null);
```
3. 在 `onMounted` 内、`EventsOn("navigate-addword", ...)` 之后增加：
```ts
    EventsOn("protocol-import", (payload: ProtocolImportPayload) => {
      protocolPayload.value = payload;
      try { Show(); } catch {}
    });
    // 冷启动兜底：主动拉取早于 EventsOn 到达的请求
    consumePendingProtocol().then((p) => {
      if (p) {
        protocolPayload.value = p;
        try { Show(); } catch {}
      }
    });
```
4. 在 `onUnmounted` 内增加：
```ts
  EventsOff("protocol-import");
```
5. `<template>` 内，加词对话框 `<AddWordPage .../>` 附近增加：
```vue
    <ProtocolImportDialog
      :payload="protocolPayload"
      @close="protocolPayload = null"
    />
```

- [ ] **Step 4: 前端格式化 + 构建**

Run: `cd wind_setting/frontend && pnpm exec prettier --write src/components/ProtocolImportDialog.vue src/App.vue src/api/wails.ts && pnpm exec vite build`
Expected: 构建成功，无类型/语法错误

---

## Task 8: 高级页注册开关区块

**Files:**
- Modify: `wind_setting/frontend/src/pages/AdvancedPage.vue`

- [ ] **Step 1: AdvancedPage.vue 增加协议关联区块**

`wind_setting/frontend/src/pages/AdvancedPage.vue`：
1. `<script setup>` import + 状态：
```ts
import {
  getProtocolStatus,
  setProtocolRegistered,
  type ProtocolRegStatus,
} from "../api/wails";

const protocolStatus = ref<ProtocolRegStatus | null>(null);

async function refreshProtocolStatus() {
  try {
    protocolStatus.value = await getProtocolStatus();
  } catch {}
}
async function toggleProtocol(enabled: boolean) {
  await setProtocolRegistered(enabled);
  await refreshProtocolStatus();
}
onMounted(refreshProtocolStatus);
```
（若文件已 import `ref`/`onMounted` 则不重复导入。）

2. `<template>` 内合适位置（参照页面已有设置项分组）增加区块：
```vue
    <section class="setting-group">
      <h3>windinput:// 链接关联</h3>
      <p class="desc">
        允许从浏览器点击 windinput:// 链接一键导入主题（后续支持方案 / 词库）。
      </p>
      <!-- macOS：系统托管，只读 -->
      <div v-if="protocolStatus?.managed" class="readonly">
        已随应用自动注册（由系统管理）
      </div>
      <!-- Windows：可开关 -->
      <label v-else class="switch-row">
        <input
          type="checkbox"
          :checked="protocolStatus?.registered"
          @change="toggleProtocol(($event.target as HTMLInputElement).checked)"
        />
        <span>关联 windinput:// 链接</span>
      </label>
    </section>
```
> 实际类名/控件请对齐 AdvancedPage.vue 已有的设置项样式与开关组件（如项目用自定义 Switch 组件则替换 `<input type=checkbox>`）。

3. 若 `advanced.search.ts` 维护搜索索引，按其格式追加本区块条目（标题「windinput:// 链接关联」+ 关键词）。

- [ ] **Step 2: 前端格式化 + 构建**

Run: `cd wind_setting/frontend && pnpm exec prettier --write src/pages/AdvancedPage.vue && pnpm exec vite build`
Expected: 构建成功

---

## Task 9: NSIS 安装/卸载注册

**Files:**
- Modify: `installer/nsis/WindInput.nsi`

- [ ] **Step 1: 安装段写协议键**

`installer/nsis/WindInput.nsi` 第 996 行（`WriteRegStr HKCU "...\Run" "WindInput"`）之后追加：
```nsi
  ; --- Step 9b: Register windinput:// URL protocol (HKCU, 装完即用) ---
  DetailPrint "正在注册 windinput:// 协议..."
  WriteRegStr HKCU "Software\Classes\windinput" "" "URL:清风输入法协议"
  WriteRegStr HKCU "Software\Classes\windinput" "URL Protocol" ""
  WriteRegStr HKCU "Software\Classes\windinput\shell\open\command" "" '"$INSTDIR\wind_setting.exe" "%1"'
```

- [ ] **Step 2: 卸载段删协议键**

`installer/nsis/WindInput.nsi` 卸载段第 1238 行（`DeleteRegValue HKCU "...\Run" "WindInput"`）之后追加：
```nsi
  ; --- 清理 windinput:// 协议键 ---
  DeleteRegKey HKCU "Software\Classes\windinput"
```

- [ ] **Step 3: 静态检查 .nsi 语法（人工核对）**

人工确认新增 `WriteRegStr`/`DeleteRegKey` 与文件既有写法一致（引号、转义、HKCU 根键）。无需编译（NSIS 在打包时校验）。

---

## Task 10: 集成验证

- [ ] **Step 1: 完整构建设置程序（Windows）**

Run: `cd wind_setting && go fmt ./... && go test ./... && go build ./... && cd frontend && pnpm exec vite build`
Expected: 测试全绿 + 编译/构建成功

- [ ] **Step 2: 手动 E2E（Windows，冷启动）**

1. 运行一次 `wind_setting.exe`（触发 self-heal 注册），关闭。
2. 浏览器地址栏输入 `windinput://import/theme?url=<urlencode 的 https 主题直链>`，回车。
3. 预期：设置程序启动 → 弹确认框显示名称/作者/来源 → 点「导入」→ Toast 成功。

- [ ] **Step 3: 手动 E2E（Windows，已开实例透传）**

1. 先打开 `wind_setting.exe` 停在任意页。
2. 浏览器再次点击 `windinput://import/theme?url=...`。
3. 预期：已有窗口被激活并弹确认框（不新开实例）。

- [ ] **Step 4: 非法链接验证**

浏览器点击 `windinput://import/theme`（缺 url）与 `windinput://import/foo?url=https://x`（未知 kind）。
预期：弹框显示明确错误提示，不崩溃。

- [ ] **Step 5: 验证收尾**

对照 `docs/design/url-schema-import.md` 第 9 节验收标准逐条确认（macOS 项需在 mac 环境验证，可单独排期）。

---

## 自查记录

- 设计 spec 各节均有对应 Task：协议注册(T2/T9)、URL 解析(T1)、接收入口(T5)、投递/pending(T3)、主题预览+导入(T4/T7)、设置开关(T8)、wails.json/Info.plist(T6)、测试(T1-T4 单测 + T10 E2E)。✅
- 类型一致性：`ProtocolRequest`/`ProtocolImportPayload`/`ProtocolRegStatus`/`ThemeURLPreview` 在 Go 与 TS 两侧字段对齐；`protocolManagedBySystem` 双平台常量；`registerProtocolAt`/`unregisterProtocolAt`/`protocolStatusAt` 命名前后一致。✅
- 无占位符：所有步骤含完整代码/命令/预期。✅
- 约束遵守：每个 Go Task 含 `go fmt` + 测试/编译；前端 Task 含 prettier + vite build；未自动 commit（遵循项目"未测试不提交"约定，提交由用户决定）。
