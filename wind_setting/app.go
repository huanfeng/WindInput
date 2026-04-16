package main

import (
	"context"
	"time"

	"github.com/huanfeng/wind_input/pkg/buildvariant"
	"github.com/huanfeng/wind_input/pkg/rpcapi"

	wailsRuntime "github.com/wailsapp/wails/v2/pkg/runtime"

	"wind_setting/internal/editor"
	"wind_setting/internal/filesync"
)

// App struct
type App struct {
	ctx context.Context

	// 启动页面（通过命令行参数指定）
	startPage string

	// 加词对话框参数
	addWordParams AddWordParams

	// 编辑器
	configEditor *editor.ConfigEditor

	// RPC 客户端（所有 IPC 操作统一走 RPC）
	rpcClient *rpcapi.Client

	// 文件监控
	fileWatcher *filesync.FileWatcher
}

// NewApp creates a new App application struct
func NewApp() *App {
	return &App{
		rpcClient: rpcapi.NewClient(),
	}
}

// GetStartPage 获取启动页面（供前端调用）
func (a *App) GetStartPage() string {
	return a.startPage
}

// GetAddWordParams 获取加词对话框参数（供前端调用）
func (a *App) GetAddWordParams() AddWordParams {
	return a.addWordParams
}

// GetVersion 获取应用版本号（供前端调用）
// Debug variant 返回 "版本号 (Debug)"
func (a *App) GetVersion() string {
	if buildvariant.IsDebug() {
		return version + " (Debug)"
	}
	return version
}

// IsDebugVariant 返回是否为调试版构建（供前端调用）
func (a *App) IsDebugVariant() bool {
	return buildvariant.IsDebug()
}

// startup is called when the app starts
func (a *App) startup(ctx context.Context) {
	a.ctx = ctx

	// 启动 IPC 监听，接收其他实例的页面切换请求
	startIPCListener(ctx)

	// 初始化编辑器
	var err error

	a.configEditor, err = editor.NewConfigEditor()
	if err == nil {
		a.configEditor.Load()
	}

	// 初始化文件监控
	a.fileWatcher = filesync.NewFileWatcher()
	if a.configEditor != nil {
		a.fileWatcher.Watch(a.configEditor.GetFilePath())
	}

	// 启动事件监听
	go a.startEventListener()
}

// shutdown is called when the app is closing
func (a *App) shutdown(ctx context.Context) {
	if a.fileWatcher != nil {
		a.fileWatcher.Stop()
	}
}

// startEventListener 启动事件监听，将 RPC 事件转发为 Wails 前端事件
func (a *App) startEventListener() {
	if a.rpcClient == nil {
		return
	}
	ctx := a.ctx
	go func() {
		for {
			err := a.rpcClient.SubscribeEvents(ctx, func(msg rpcapi.EventMessage) {
				wailsRuntime.EventsEmit(a.ctx, "dict-event", map[string]string{
					"type":      msg.Type,
					"schema_id": msg.SchemaID,
					"action":    msg.Action,
				})
			})
			if err != nil {
				select {
				case <-ctx.Done():
					return
				default:
					// 连接断开，延迟重试
					time.Sleep(2 * time.Second)
				}
			}
		}
	}()
}
