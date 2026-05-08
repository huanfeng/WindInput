package main

import (
	"wind_setting/updater"

	wailsRuntime "github.com/wailsapp/wails/v2/pkg/runtime"
)

// GetUpdateConfig 返回当前更新设置。
func (a *App) GetUpdateConfig() updater.Config {
	return updater.LoadConfig()
}

// SaveUpdateConfig 持久化更新设置。
func (a *App) SaveUpdateConfig(cfg updater.Config) error {
	return updater.SaveConfig(cfg)
}

// CheckUpdate 获取最新 Release 并与当前构建版本比较。
func (a *App) CheckUpdate() (*updater.CheckResult, error) {
	return updater.CheckUpdate(version)
}

// StartDownload 在 goroutine 中开始下载安装包。
// expectedSize 为资产字节数，用于判断本地缓存是否完整；传 0 则不校验。
// 进度通过 Wails 事件 "update:progress" 推送（payload: DownloadProgress）。
// 完成时触发 "update:done"（payload: 安装包本地路径字符串）。
// 失败时触发 "update:error"（payload: 错误信息字符串）。
func (a *App) StartDownload(downloadURL, assetName string, expectedSize int64) {
	go func() {
		path, err := updater.DownloadRelease(downloadURL, assetName, expectedSize, func(p updater.DownloadProgress) {
			wailsRuntime.EventsEmit(a.ctx, "update:progress", p)
		})
		if err != nil {
			wailsRuntime.EventsEmit(a.ctx, "update:error", err.Error())
			return
		}
		wailsRuntime.EventsEmit(a.ctx, "update:done", path)
	}()
}

// GetPendingUpdate 返回启动时自动检查到的更新结果（如有）并清除。
// 前端在注册事件监听后主动调用，避免 emit 比 EventsOn 注册更早导致的丢失。
func (a *App) GetPendingUpdate() *updater.CheckResult {
	a.startupUpdateMu.Lock()
	defer a.startupUpdateMu.Unlock()
	r := a.startupUpdateResult
	a.startupUpdateResult = nil
	return r
}

// CancelDownload 取消正在进行的下载。
func (a *App) CancelDownload() {
	updater.CancelDownload()
}

// InstallRelease 运行 NSIS 安装程序。silent=true 时静默安装（/S），否则显示安装界面。
func (a *App) InstallRelease(installerPath string, silent bool) error {
	return updater.InstallRelease(installerPath, silent)
}
