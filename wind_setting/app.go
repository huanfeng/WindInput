package main

import (
	"context"
	"sync"
	"time"

	"github.com/huanfeng/wind_input/pkg/buildvariant"
	"github.com/huanfeng/wind_input/pkg/rpcapi"
	"wind_setting/updater"

	wailsRuntime "github.com/wailsapp/wails/v2/pkg/runtime"
)

// App struct
type App struct {
	ctx context.Context

	// 启动页面（通过命令行参数指定）
	startPage string

	// 加词对话框参数
	addWordParams AddWordParams

	// RPC 客户端（所有 IPC 操作统一走 RPC）
	rpcClient *rpcapi.Client

	// 启动时自动检查到的更新结果，供前端主动拉取（避免 emit 比 EventsOn 注册更早）
	startupUpdateMu     sync.Mutex
	startupUpdateResult *updater.CheckResult
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

	// 启动事件监听
	go a.startEventListener()

	// 若用户已同意联网且开启了自动检查，后台静默检查更新
	updateCfg := updater.LoadConfig()
	if updateCfg.NetworkConsent && updateCfg.AutoCheck {
		go a.runStartupUpdateCheck()
	}
}

// shutdown is called when the app is closing
func (a *App) shutdown(ctx context.Context) {}

// runStartupUpdateCheck 后台静默检查更新，有新版本时：
// 1. 存入 startupUpdateResult 供前端主动拉取（GetPendingUpdate）
// 2. 同时 emit 事件（若前端已注册则直接触发，否则依赖拉取兜底）
func (a *App) runStartupUpdateCheck() {
	result, err := updater.CheckUpdate(version)
	if err != nil || !result.HasUpdate {
		return
	}
	a.startupUpdateMu.Lock()
	a.startupUpdateResult = result
	a.startupUpdateMu.Unlock()
	wailsRuntime.EventsEmit(a.ctx, "update:available", result)
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
				payload := map[string]string{
					"type":      string(msg.Type),
					"schema_id": msg.SchemaID,
					"action":    string(msg.Action),
				}
				switch msg.Type {
				case rpcapi.EventTypeConfig:
					wailsRuntime.EventsEmit(a.ctx, rpcapi.WailsEventConfig, payload)
				case rpcapi.EventTypeStats:
					wailsRuntime.EventsEmit(a.ctx, rpcapi.WailsEventStats, payload)
				case rpcapi.EventTypeSystem:
					wailsRuntime.EventsEmit(a.ctx, rpcapi.WailsEventSystem, payload)
				default:
					wailsRuntime.EventsEmit(a.ctx, rpcapi.WailsEventDict, payload)
				}
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
