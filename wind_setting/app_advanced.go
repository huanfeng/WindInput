package main

import (
	"fmt"
	"time"

	"github.com/huanfeng/wind_input/pkg/rpcapi"
	wailsRuntime "github.com/wailsapp/wails/v2/pkg/runtime"
)

// BackupPreviewResult 备份预览结果
type BackupPreviewResult struct {
	Preview *rpcapi.BackupPreview `json:"preview,omitempty"`
	Error   string                `json:"error,omitempty"`
}

// RestorePreviewResult 还原预览结果
type RestorePreviewResult struct {
	Cancelled bool                   `json:"cancelled"`
	ZipPath   string                 `json:"zip_path,omitempty"`
	Preview   *rpcapi.RestorePreview `json:"preview,omitempty"`
	Error     string                 `json:"error,omitempty"`
}

// BackupResult 备份操作结果
type BackupResult struct {
	Cancelled bool   `json:"cancelled"`
	Error     string `json:"error,omitempty"`
}

// GetBackupPreview 获取当前数据统计（备份前预览）
func (a *App) GetBackupPreview() BackupPreviewResult {
	preview, err := a.rpcClient.SystemPreviewBackup()
	if err != nil {
		return BackupPreviewResult{Error: err.Error()}
	}
	return BackupPreviewResult{Preview: preview}
}

// BackupData 弹出保存对话框并执行备份
func (a *App) BackupData() BackupResult {
	defaultName := "WindInput_backup_" + time.Now().Format("2006-01-02") + ".zip"
	zipPath, err := wailsRuntime.SaveFileDialog(a.ctx, wailsRuntime.SaveDialogOptions{
		Title:           "选择备份保存位置",
		DefaultFilename: defaultName,
		Filters: []wailsRuntime.FileFilter{
			{DisplayName: "备份文件 (*.zip)", Pattern: "*.zip"},
		},
	})
	if err != nil {
		return BackupResult{Error: fmt.Sprintf("对话框错误: %v", err)}
	}
	if zipPath == "" {
		return BackupResult{Cancelled: true}
	}
	if _, err := a.rpcClient.SystemBackup(zipPath); err != nil {
		return BackupResult{Error: fmt.Sprintf("备份失败: %v", err)}
	}
	return BackupResult{}
}

// GetRestorePreview 弹出打开对话框并返回备份文件预览
func (a *App) GetRestorePreview() RestorePreviewResult {
	zipPath, err := wailsRuntime.OpenFileDialog(a.ctx, wailsRuntime.OpenDialogOptions{
		Title: "选择备份文件",
		Filters: []wailsRuntime.FileFilter{
			{DisplayName: "备份文件 (*.zip)", Pattern: "*.zip"},
		},
	})
	if err != nil {
		return RestorePreviewResult{Error: fmt.Sprintf("对话框错误: %v", err)}
	}
	if zipPath == "" {
		return RestorePreviewResult{Cancelled: true}
	}
	preview, err := a.rpcClient.SystemPreviewRestore(zipPath)
	if err != nil {
		return RestorePreviewResult{Error: fmt.Sprintf("读取备份信息失败: %v", err)}
	}
	return RestorePreviewResult{ZipPath: zipPath, Preview: preview}
}

// RestoreData 执行数据还原，返回空字符串表示成功，否则返回错误信息
func (a *App) RestoreData(zipPath string) string {
	if err := a.rpcClient.SystemRestore(zipPath); err != nil {
		return err.Error()
	}
	return ""
}

// ResetData 清除所有用户数据，返回空字符串表示成功，否则返回错误信息
func (a *App) ResetData() string {
	if err := a.rpcClient.SystemReset(); err != nil {
		return err.Error()
	}
	return ""
}
