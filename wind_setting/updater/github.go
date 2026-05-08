package updater

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"
)

const (
	repoOwner = "huanfeng"
	repoName  = "WindInput"
)

// apiMirrors 按优先级列出 GitHub API 基础 URL。
var apiMirrors = []string{
	"https://api.github.com",
	"https://api.kkgithub.com",
}

// downloadMirrorPrefixes 下载 URL 镜像前缀列表（index 0 为直连）。
var downloadMirrorPrefixes = []string{
	"",
	"https://ghproxy.com/",
	"https://mirror.ghproxy.com/",
}

// ReleaseAsset 是 GitHub Release 中的单个下载文件。
type ReleaseAsset struct {
	Name               string `json:"name"`
	BrowserDownloadURL string `json:"browser_download_url"`
	Size               int64  `json:"size"`
}

// ReleaseInfo 是 GitHub Release API 的关键字段。
type ReleaseInfo struct {
	TagName    string         `json:"tag_name"`
	Name       string         `json:"name"`
	Body       string         `json:"body"`
	HTMLURL    string         `json:"html_url"`
	Assets     []ReleaseAsset `json:"assets"`
	PreRelease bool           `json:"prerelease"`
}

// SetupAsset 返回第一个以 "-Setup.exe" 结尾的 Asset，找不到则返回 nil。
func (r *ReleaseInfo) SetupAsset() *ReleaseAsset {
	for i := range r.Assets {
		if strings.HasSuffix(r.Assets[i].Name, "-Setup.exe") {
			return &r.Assets[i]
		}
	}
	return nil
}

// FetchLatestRelease 查询 GitHub Releases API，镜像依次 fallback。
// 每个镜像超时 10 秒；所有镜像均失败时返回错误。
func FetchLatestRelease() (*ReleaseInfo, error) {
	client := newHTTPClient()
	path := fmt.Sprintf("/repos/%s/%s/releases/latest", repoOwner, repoName)

	var lastErr error
	for _, base := range apiMirrors {
		info, err := fetchRelease(client, base+path)
		if err == nil {
			return info, nil
		}
		lastErr = fmt.Errorf("%s: %w", base, err)
	}
	return nil, fmt.Errorf("所有镜像均无法访问: %w", lastErr)
}

func fetchRelease(client *http.Client, apiURL string) (*ReleaseInfo, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	req, err := http.NewRequestWithContext(ctx, http.MethodGet, apiURL, nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Accept", "application/vnd.github+json")
	req.Header.Set("User-Agent", "WindInput-Updater/1.0")

	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("HTTP %d", resp.StatusCode)
	}
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, err
	}
	var info ReleaseInfo
	if err := json.Unmarshal(body, &info); err != nil {
		return nil, err
	}
	return &info, nil
}

// MirroredURL 返回带镜像前缀的下载地址。mirrorIndex=0 为直连。
func MirroredURL(originalURL string, mirrorIndex int) string {
	if mirrorIndex <= 0 || mirrorIndex >= len(downloadMirrorPrefixes) {
		return originalURL
	}
	return downloadMirrorPrefixes[mirrorIndex] + originalURL
}
