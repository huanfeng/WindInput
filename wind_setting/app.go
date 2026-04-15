package main

import (
	"context"
	"path/filepath"

	"github.com/huanfeng/wind_input/pkg/buildvariant"
	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/control"
	"github.com/huanfeng/wind_input/pkg/rpcapi"

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
	configEditor           *editor.ConfigEditor
	phraseEditor           *editor.PhraseEditor // 用户短语编辑器
	systemPhraseEditor     *editor.PhraseEditor // 系统短语编辑器（程序目录，只读）
	systemUserPhraseEditor *editor.PhraseEditor // 用户目录的系统短语（修改后的副本）

	// RPC 客户端（词库/Shadow 操作走 RPC）
	rpcClient *rpcapi.Client

	// 文件监控
	fileWatcher *filesync.FileWatcher

	// 控制管道客户端
	controlClient *control.Client
}

// NewApp creates a new App application struct
func NewApp() *App {
	return &App{
		controlClient: control.NewClient(),
		rpcClient:     rpcapi.NewClient(),
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

	a.phraseEditor, err = editor.NewPhraseEditor()
	if err == nil {
		a.phraseEditor.Load()
	}

	// 初始化系统短语编辑器（只读，从 exe/data 目录加载）
	systemPhrasePath := filepath.Join(getExeDir(), "data", "system.phrases.yaml")
	a.systemPhraseEditor = editor.NewPhraseEditorWithPath(systemPhrasePath)
	a.systemPhraseEditor.Load()

	// 初始化用户目录的系统短语编辑器（同名覆盖）
	systemUserPath, _ := config.GetSystemPhrasesUserPath()
	if systemUserPath != "" {
		a.systemUserPhraseEditor = editor.NewPhraseEditorWithPath(systemUserPath)
		a.systemUserPhraseEditor.Load() // 文件可能不存在，Load 会返回错误但不影响
	}

	// 初始化文件监控
	a.fileWatcher = filesync.NewFileWatcher()
	if a.configEditor != nil {
		a.fileWatcher.Watch(a.configEditor.GetFilePath())
	}
	if a.phraseEditor != nil {
		a.fileWatcher.Watch(a.phraseEditor.GetFilePath())
	}
}

// shutdown is called when the app is closing
func (a *App) shutdown(ctx context.Context) {
	if a.fileWatcher != nil {
		a.fileWatcher.Stop()
	}
}
