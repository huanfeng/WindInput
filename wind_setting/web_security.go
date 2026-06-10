package main

import (
	"net"
	"net/http"
	"net/url"
	"strconv"
	"strings"
)

// 本文件集中放置 Web 形态（HTTP 反射网关 + 主题服务）的服务端安全控制。
//
// 威胁模型：两个本地 HTTP 服务（web_server / theme_server）监听 127.0.0.1，但
// 「监听回环」并不等于「只有本机可信代码能访问」——浏览器里的任意网页都能向
// localhost 发请求。需要防御：
//   1. 恶意网页跨站请求（CSRF）：用户浏览 evil.com 时，其 JS fetch 本地服务。
//   2. DNS rebinding：把 evil.com 解析到 127.0.0.1 绕过同源策略。
//   3. 前端黑名单被绕过：webShim.ts 的 DESKTOP_ONLY_METHODS 是客户端检查，
//      直接 curl /api/call 即可绕过，服务端必须独立强制。
//
// 注意：这些控制只作用于 Web 形态的 HTTP 服务，桌面 Wails 形态经 WebView 绑定
// 调用 *App，完全不走 HTTP，故不受影响。

// isLoopbackHost 判断 HTTP 请求的 Host 头是否指向本机回环地址。
//
// 这是抵御 DNS rebinding 的核心：攻击者把自己控制的域名解析到 127.0.0.1，诱导
// 用户浏览器向本地服务发请求绕过同源策略；但此时浏览器请求的 Host 头是攻击者
// 域名而非回环地址，据此即可拦截。合法访问（用户打开 http://127.0.0.1:<port>
// 或 http://localhost:<port>）的 Host 头一定是回环地址。
func isLoopbackHost(host string) bool {
	h := host
	if hh, _, err := net.SplitHostPort(host); err == nil {
		h = hh
	}
	h = strings.ToLower(strings.TrimSuffix(strings.TrimPrefix(h, "["), "]"))
	switch h {
	case "127.0.0.1", "localhost", "::1":
		return true
	}
	if ip := net.ParseIP(h); ip != nil {
		return ip.IsLoopback()
	}
	return false
}

// webCallDenied 是禁止经 Web 反射网关（/api/call）调用的方法集合。
//
// 服务端独立强制，不依赖前端 webShim.ts 的 DESKTOP_ONLY_METHODS（后者可被绕过）。
// 收录两类：
//   - 接受任意文件路径 → 经网关可传入任意绝对路径，构成任意文件读/写（信息泄露/
//     篡改）。这些方法在 Web 形态本就走不通（选路径依赖 openFileDialog，webMode 下
//     已返回错误），拉黑不影响可用功能。
//   - 不可逆的全局破坏操作。
//
// 维护规则：新增「接受路径参数」或「破坏性」且不应经 Web 暴露的 App 方法时，
// 必须在此登记。
var webCallDenied = map[string]bool{
	// 任意文件路径读 → 信息泄露
	"PreviewImportFile": true,
	"PreviewZipImport":  true,
	"PreviewTextList":   true,
	// 任意文件路径读 + 写入词库
	"ExecuteImport":    true,
	"ExecuteZipImport": true,
	// 任意文件路径（zip）还原 → 任意文件读 + 覆盖用户数据
	"RestoreData": true,
	// 不可逆全局清空
	"ResetData": true,
}

// isSelfOrigin 判断 Origin 头是否为本服务自身来源（同源）。
func (ws *webServer) isSelfOrigin(origin string) bool {
	port := strconv.Itoa(ws.port)
	for _, host := range []string{"127.0.0.1", "localhost"} {
		if origin == "http://"+host+":"+port {
			return true
		}
	}
	return false
}

// guard 是 /api/* 端点的安全中间件，抵御恶意网页与 DNS rebinding。
//
// 判据：
//   - Host 必须指向回环地址（防 DNS rebinding）。
//   - Sec-Fetch-Site 由浏览器强制设置且网页脚本无法伪造：合法调用来自同源 SPA
//     （same-origin）；cross-site / same-site 一律拒绝。非浏览器客户端不发此头，
//     回退到 Origin 校验。
//   - Origin 若存在必须同源（覆盖不发 Sec-Fetch-Site 的旧浏览器）。
func (ws *webServer) guard(next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if !isLoopbackHost(r.Host) {
			http.Error(w, "forbidden: non-loopback host", http.StatusForbidden)
			return
		}
		switch r.Header.Get("Sec-Fetch-Site") {
		case "same-origin", "none", "":
			// 允许；"" / "none" 进一步由下面的 Origin 校验兜底
		default: // cross-site, same-site
			http.Error(w, "forbidden: cross-site request", http.StatusForbidden)
			return
		}
		if origin := r.Header.Get("Origin"); origin != "" && !ws.isSelfOrigin(origin) {
			http.Error(w, "forbidden: cross-origin request", http.StatusForbidden)
			return
		}
		next(w, r)
	}
}

// isAllowedThemeOrigin 判断 Origin 是否允许跨域访问主题服务（theme_server）。
//
// 放行：
//   - windinput.com 及其任意子域（生产环境，要求 https；后端 API 来源可能是不同
//     子域，故按子域通配而非固定单一 origin）。
//   - localhost / 127.0.0.1 / ::1（本地调试，不限 scheme 与端口）。
//
// 其余一律拒绝。主题编辑器是跨域网页，必带 Origin；无 Origin 的请求（同源或非
// 浏览器工具）不经此函数，由 corsMiddleware 的 Host 回环校验兜底。
func isAllowedThemeOrigin(origin string) bool {
	u, err := url.Parse(origin)
	if err != nil || u.Host == "" {
		return false
	}
	switch u.Hostname() {
	case "localhost", "127.0.0.1", "::1":
		return true
	}
	host := u.Hostname()
	if u.Scheme == "https" && (host == "windinput.com" || strings.HasSuffix(host, ".windinput.com")) {
		return true
	}
	return false
}
