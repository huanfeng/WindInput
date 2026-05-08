package updater

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"sync"
	"sync/atomic"
	"time"
)

// CheckResult 是 CheckUpdate 的返回值。
type CheckResult struct {
	HasUpdate      bool   `json:"has_update"`
	LatestVersion  string `json:"latest_version"`
	CurrentVersion string `json:"current_version"`
	ReleaseNotes   string `json:"release_notes"`
	ReleaseURL     string `json:"release_url"`
	AssetName      string `json:"asset_name"`
	AssetSize      int64  `json:"asset_size"`
	DownloadURL    string `json:"download_url"`
}

// DownloadProgress 下载进度，周期性通过回调推送。
type DownloadProgress struct {
	Downloaded int64   `json:"downloaded"`
	Total      int64   `json:"total"`
	Percent    float64 `json:"percent"`
}

// ProgressCallback 下载进度回调函数类型。
type ProgressCallback func(DownloadProgress)

// CheckUpdate 获取最新 Release 并与 currentVersion 比较。
func CheckUpdate(currentVersion string) (*CheckResult, error) {
	release, err := FetchLatestRelease()
	if err != nil {
		return nil, err
	}
	current := ParseVersion(currentVersion)
	latest := ParseVersion(release.TagName)
	result := &CheckResult{
		HasUpdate:      latest.IsNewerThan(current),
		LatestVersion:  release.TagName,
		CurrentVersion: currentVersion,
		ReleaseNotes:   release.Body,
		ReleaseURL:     release.HTMLURL,
	}
	if asset := release.SetupAsset(); asset != nil {
		result.AssetName = asset.Name
		result.AssetSize = asset.Size
		result.DownloadURL = asset.BrowserDownloadURL
	}
	return result, nil
}

var (
	activeCancel atomic.Pointer[context.CancelFunc]
	downloadMu   sync.Mutex
)

// DownloadRelease 将安装包下载到 %TEMP%\<assetName>。
// expectedSize 为 GitHub API 返回的资产大小（字节），用于验证已缓存文件是否完整；传 0 则跳过校验。
// 通过 progressFn 周期推送进度，依次尝试各镜像直连/代理。
// 返回本地文件路径。
func DownloadRelease(downloadURL, assetName string, expectedSize int64, progressFn ProgressCallback) (string, error) {
	downloadMu.Lock()
	defer downloadMu.Unlock()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Minute)
	activeCancel.Store(&cancel)
	defer func() {
		cancel()
		activeCancel.Store(nil)
	}()

	dest := filepath.Join(os.TempDir(), assetName)

	// 已存在且大小吻合则跳过下载，直接推送 100% 进度
	if fi, err := os.Stat(dest); err == nil && fi.Size() > 0 {
		if expectedSize <= 0 || fi.Size() == expectedSize {
			sz := fi.Size()
			if progressFn != nil {
				progressFn(DownloadProgress{Downloaded: sz, Total: sz, Percent: 100})
			}
			return dest, nil
		}
		// 大小不符（下载不完整），删除后重新下载
		os.Remove(dest)
	}

	client := newHTTPClient()

	var lastErr error
	for i := range downloadMirrorPrefixes {
		mirrorURL := MirroredURL(downloadURL, i)
		err := downloadFile(ctx, client, mirrorURL, dest, progressFn)
		if err == nil {
			return dest, nil
		}
		lastErr = err
		if errors.Is(ctx.Err(), context.Canceled) {
			return "", fmt.Errorf("下载已取消")
		}
	}
	return "", fmt.Errorf("下载失败: %w", lastErr)
}

// CancelDownload 取消正在进行的下载。
func CancelDownload() {
	if p := activeCancel.Load(); p != nil {
		(*p)()
	}
}

func downloadFile(ctx context.Context, client *http.Client, rawURL, dest string, progressFn ProgressCallback) error {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, rawURL, nil)
	if err != nil {
		return err
	}
	req.Header.Set("User-Agent", "WindInput-Updater/1.0")

	resp, err := client.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("HTTP %d", resp.StatusCode)
	}

	f, err := os.CreateTemp(filepath.Dir(dest), "wind_update_*.tmp")
	if err != nil {
		return err
	}
	tmpPath := f.Name()

	total := resp.ContentLength
	var downloaded int64
	buf := make([]byte, 32*1024)
	lastReport := time.Now()

	for {
		n, readErr := resp.Body.Read(buf)
		if n > 0 {
			if _, werr := f.Write(buf[:n]); werr != nil {
				f.Close()
				os.Remove(tmpPath)
				return werr
			}
			downloaded += int64(n)
			if progressFn != nil && time.Since(lastReport) > 200*time.Millisecond {
				pct := 0.0
				if total > 0 {
					pct = float64(downloaded) / float64(total) * 100
				}
				progressFn(DownloadProgress{Downloaded: downloaded, Total: total, Percent: pct})
				lastReport = time.Now()
			}
		}
		if readErr == io.EOF {
			break
		}
		if readErr != nil {
			f.Close()
			os.Remove(tmpPath)
			return readErr
		}
	}

	// Emit final 100% progress
	if progressFn != nil && total > 0 {
		progressFn(DownloadProgress{Downloaded: downloaded, Total: total, Percent: 100})
	}

	if err := f.Close(); err != nil {
		os.Remove(tmpPath)
		return err
	}

	if err := os.Rename(tmpPath, dest); err != nil {
		copyErr := copyFile(tmpPath, dest)
		os.Remove(tmpPath)
		return copyErr
	}
	return nil
}

func copyFile(src, dst string) error {
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()
	out, err := os.Create(dst)
	if err != nil {
		return err
	}
	_, copyErr := io.Copy(out, in)
	closeErr := out.Close()
	if copyErr != nil {
		os.Remove(dst)
		return copyErr
	}
	return closeErr
}

// InstallRelease 启动 NSIS 安装程序。silent=true 时传入 /QUIET 参数（显示进度界面但自动安装），否则显示完整交互界面。
func InstallRelease(installerPath string, silent bool) error {
	var cmd *exec.Cmd
	if silent {
		cmd = exec.Command(installerPath, "/QUIET")
	} else {
		cmd = exec.Command(installerPath)
	}
	return cmd.Start()
}
