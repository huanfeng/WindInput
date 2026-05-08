//go:build windows

package updater

import (
	"net/http"
	"net/url"
	"strings"

	"golang.org/x/sys/windows/registry"
)

// systemProxyURL 从 Windows 注册表读取 IE/WinINet 代理设置。
// 未配置代理或读取失败时返回 nil。
func systemProxyURL() *url.URL {
	key, err := registry.OpenKey(
		registry.CURRENT_USER,
		`Software\Microsoft\Windows\CurrentVersion\Internet Settings`,
		registry.QUERY_VALUE,
	)
	if err != nil {
		return nil
	}
	defer key.Close()

	enabled, _, err := key.GetIntegerValue("ProxyEnable")
	if err != nil || enabled == 0 {
		return nil
	}
	proxyServer, _, err := key.GetStringValue("ProxyServer")
	if err != nil || proxyServer == "" {
		return nil
	}

	// ProxyServer 格式可能是 "host:port" 或 "http=host:port;https=host:port;..."
	server := proxyServer
	for _, part := range strings.Split(proxyServer, ";") {
		scheme, addr, ok := strings.Cut(part, "=")
		if ok && (scheme == "http" || scheme == "https") {
			server = addr
			break
		}
	}
	if !strings.Contains(server, "://") {
		server = "http://" + server
	}
	u, err := url.Parse(server)
	if err != nil {
		return nil
	}
	return u
}

// newHTTPClient 返回配置了系统代理的 http.Client。
// 若未检测到系统代理则 fallback 到环境变量代理。
func newHTTPClient() *http.Client {
	transport := &http.Transport{}
	if proxyURL := systemProxyURL(); proxyURL != nil {
		transport.Proxy = http.ProxyURL(proxyURL)
	} else {
		transport.Proxy = http.ProxyFromEnvironment
	}
	return &http.Client{Transport: transport}
}
