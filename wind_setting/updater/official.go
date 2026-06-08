package updater

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"runtime"
	"time"
)

const officialBaseURL = "https://dl.windinput.com"

type officialLatestJSON struct {
	Version         string `json:"version"`
	Tag             string `json:"tag"`
	ExeURL          string `json:"exeUrl"`
	PkgURL          string `json:"pkgUrl"`
	ReleaseNotesURL string `json:"releaseNotesUrl"`
	PublishedAt     string `json:"publishedAt"`
}

// FetchOfficialLatest 从官网 CDN 获取最新版本信息，转换为 ReleaseInfo。
func FetchOfficialLatest() (*ReleaseInfo, error) {
	client := newHTTPClient()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	req, err := http.NewRequestWithContext(ctx, http.MethodGet, officialBaseURL+"/latest.json", nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("User-Agent", "WindInput-Updater/1.0")
	req.Header.Set("Cache-Control", "no-cache")

	resp, err := client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("官网升级服务不可达: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("官网升级服务返回 HTTP %d", resp.StatusCode)
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, err
	}

	var latest officialLatestJSON
	if err := json.Unmarshal(body, &latest); err != nil {
		return nil, fmt.Errorf("latest.json 格式错误: %w", err)
	}
	if latest.Version == "" {
		return nil, fmt.Errorf("latest.json 缺少版本字段")
	}

	tag := latest.Tag
	if tag == "" {
		tag = "v" + latest.Version
	}

	releaseNotes := fetchOfficialReleaseNotes(client, latest.ReleaseNotesURL)

	var assets []ReleaseAsset
	switch runtime.GOOS {
	case "darwin":
		if latest.PkgURL != "" {
			assets = []ReleaseAsset{{
				Name:               "WindInput-" + latest.Version + "-macOS.pkg",
				BrowserDownloadURL: latest.PkgURL,
			}}
		}
	default: // windows
		if latest.ExeURL != "" {
			assets = []ReleaseAsset{{
				Name:               "WindInput-" + latest.Version + "-Setup.exe",
				BrowserDownloadURL: latest.ExeURL,
			}}
		}
	}

	return &ReleaseInfo{
		TagName: tag,
		Name:    "清风输入法 v" + latest.Version,
		Body:    releaseNotes,
		HTMLURL: "https://github.com/" + repoOwner + "/" + repoName + "/releases/tag/" + tag,
		Assets:  assets,
	}, nil
}

func fetchOfficialReleaseNotes(client *http.Client, notesURL string) string {
	if notesURL == "" {
		return ""
	}
	ctx, cancel := context.WithTimeout(context.Background(), 8*time.Second)
	defer cancel()

	req, err := http.NewRequestWithContext(ctx, http.MethodGet, notesURL, nil)
	if err != nil {
		return ""
	}
	req.Header.Set("User-Agent", "WindInput-Updater/1.0")

	resp, err := client.Do(req)
	if err != nil {
		return ""
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return ""
	}

	b, err := io.ReadAll(resp.Body)
	if err != nil {
		return ""
	}
	return string(b)
}
